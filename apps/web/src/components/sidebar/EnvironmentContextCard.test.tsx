// @vitest-environment happy-dom
import { act, cloneElement, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { EnvironmentId } from "@coddy/contracts";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  environment: null as Record<string, unknown> | null,
  activeEnvironmentId: null as string | null,
  navigate: vi.fn(),
  commandCalls: [] as Array<{ label?: string; input: unknown }>,
  menuItems: [] as Array<Record<string, unknown>>,
  compatVerdict: null as unknown,
  reset() {
    h.environment = null;
    h.compatVerdict = null;
    h.activeEnvironmentId = null;
    h.navigate.mockReset();
    h.commandCalls = [];
    h.menuItems = [];
  },
}));

vi.mock("../../state/entities", () => ({
  useActiveEnvironmentId: () => h.activeEnvironmentId,
}));
vi.mock("../../state/environments", () => ({
  useEnvironment: () => h.environment,
}));
vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: { label?: string }) => (input: unknown) => {
    h.commandCalls.push(command.label === undefined ? { input } : { label: command.label, input });
    return Promise.resolve({ _tag: "Success", value: undefined });
  },
}));
vi.mock("../../connection/catalog", () => ({
  environmentCatalog: { disconnect: { label: "environment-catalog:disconnect" } },
}));
vi.mock("../../connection/environmentCompat", () => ({
  resolveEnvironmentCompatVerdict: () => h.compatVerdict,
  selectRemoteUpdateControlCapability: (serverConfig: unknown) => serverConfig !== null,
}));
vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => h.navigate,
}));
vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuPopup: ({ children }: { children?: ReactNode }) => <>{children}</>,
  MenuTrigger: ({ children }: { render?: ReactElement; children?: ReactNode }) => <>{children}</>,
  MenuItem: (props: Record<string, unknown>) => {
    h.menuItems.push(props);
    return null;
  },
}));
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children?: ReactNode }) => <>{children}</>,
  TooltipPopup: ({ children }: { children?: ReactNode }) => <span data-tooltip="">{children}</span>,
  TooltipTrigger: ({ render, children }: { render?: ReactElement; children?: ReactNode }) =>
    render ? cloneElement(render, undefined, children) : <>{children}</>,
}));

import { EnvironmentContextCard } from "./EnvironmentContextCard";

const ENV_REMOTE = EnvironmentId.make("env-remote");
const CONNECTED = { phase: "connected", error: null, traceId: null } as const;
/** The liveness monitor's disconnect reason (client-runtime connection supervisor). */
const NO_DATA_REASON =
  "No data from AI-SERVER for 30 seconds. The connection is too slow or was lost.";
const NO_DATA_STATUS = `${NO_DATA_REASON} Reconnecting…`;
/** Reconnecting after the liveness monitor gave up on a silent link. */
const RECONNECTING_NO_DATA = {
  phase: "reconnecting",
  error: NO_DATA_REASON,
  traceId: null,
} as const;

function remoteEnvironment(
  serverConfig: unknown = null,
  connection: { phase: string; error: string | null; traceId: string | null } = CONNECTED,
) {
  return {
    environmentId: ENV_REMOTE,
    label: "AI-SERVER",
    entry: { target: { _tag: "BearerConnectionTarget", connectionId: "paired-1" } },
    connection,
    serverConfig,
  };
}

const SERVER_CONFIG = { environment: { serverVersion: "0.6.2", capabilities: {} } };

let root: Root | null = null;
let container: HTMLDivElement | null = null;

afterEach(() => {
  act(() => root?.unmount());
  container?.remove();
  root = null;
  container = null;
});

/** Mounts the card into the document so focus and layout attributes are real DOM. */
function mountCard(): HTMLDivElement {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  act(() => root!.render(<EnvironmentContextCard />));
  return container;
}

/** The innermost element holding the whole status text, outside any tooltip popup. */
function statusLine(host: ParentNode, statusText: string): HTMLElement {
  const holders = [...host.querySelectorAll<HTMLElement>("*")].filter(
    (element) =>
      element.closest("[data-tooltip]") === null &&
      (element.textContent ?? "").includes(statusText),
  );
  const line = holders.at(-1);
  expect(line, "the card renders its status line").toBeDefined();
  return line!;
}

/** The text assistive technology reads: text content minus aria-hidden decoration. */
function accessibleText(element: Element): string {
  const copy = element.cloneNode(true) as Element;
  for (const hidden of copy.querySelectorAll('[aria-hidden="true"]')) hidden.remove();
  return (copy.textContent ?? "").replace(/\s+/gu, " ").trim();
}

describe("EnvironmentContextCard", () => {
  it("disables checking while the shared update is already queued or running", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.7.2", capabilities: {} },
    });
    renderToStaticMarkup(<EnvironmentContextCard onCheckForUpdates={vi.fn()} updateInProgress />);
    expect(h.menuItems.find((entry) => entry.children === "Check for updates")?.disabled).toBe(
      true,
    );
  });
  it("offers manual update steps instead of a check that cannot update this host", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: {
        serverVersion: "0.7.2",
        remoteUpdateSupport: { installMode: "manual" },
        capabilities: {},
      },
    });
    const show = vi.fn();
    renderToStaticMarkup(
      <EnvironmentContextCard onCheckForUpdates={vi.fn()} onShowUpdateSteps={show} />,
    );
    const item = h.menuItems.find((entry) => entry.children === "Show update steps");
    expect(item).toBeDefined();
    expect(h.menuItems.some((entry) => entry.children === "Check for updates")).toBe(false);
    (item!.onClick as () => void)();
    expect(show).toHaveBeenCalledExactlyOnceWith(ENV_REMOTE);
  });
  it("renders nothing for Local", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = {
      ...remoteEnvironment(),
      entry: { target: { _tag: "PrimaryConnectionTarget" } },
    };
    expect(renderToStaticMarkup(<EnvironmentContextCard />)).toBe("");
  });

  it("renders name, status, and version line for a remote", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    const markup = renderToStaticMarkup(<EnvironmentContextCard />);
    expect(markup).toContain("AI-SERVER");
    expect(markup).toContain("Connected");
    expect(markup).toContain("Coddy Studio v0.4.2");
  });

  it("disconnects through the latch and deep-links Manage", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    renderToStaticMarkup(<EnvironmentContextCard />);
    const labels = h.menuItems.map((item) => item.children);
    expect(labels).toContain("Disconnect");
    expect(labels).toContain("Manage…");
    const disconnect = h.menuItems.find((item) => item.children === "Disconnect");
    expect(disconnect).toBeDefined();
    (disconnect!.onClick as () => void)();
    expect(h.commandCalls).toEqual([
      { label: "environment-catalog:disconnect", input: ENV_REMOTE },
    ]);
    const manage = h.menuItems.find((item) => item.children === "Manage…");
    expect(manage).toBeDefined();
    (manage!.onClick as () => void)();
    expect(h.navigate).toHaveBeenCalledWith({ to: "/settings/remote-servers" });
  });

  it("hides update checks until a capable handler is injected", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    renderToStaticMarkup(<EnvironmentContextCard />);
    expect(h.menuItems.map((item) => item.children)).not.toContain("Check for updates");

    h.menuItems = [];
    const onCheckForUpdates = vi.fn();
    renderToStaticMarkup(<EnvironmentContextCard onCheckForUpdates={onCheckForUpdates} />);
    const item = h.menuItems.find((entry) => entry.children === "Check for updates");
    expect(item).toBeDefined();
    (item!.onClick as () => void)();
    expect(onCheckForUpdates).toHaveBeenCalledWith(ENV_REMOTE);
  });

  it("keeps its secondary text at the 12 px floor", () => {
    h.reset();
    h.compatVerdict = { kind: "legacy" };
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    const markup = renderToStaticMarkup(
      <EnvironmentContextCard updateBadge={<span data-testid="update-badge">Up to date</span>} />,
    );
    expect(markup).not.toMatch(/text-\[(?:\d|1[01])px\]/u);
    expect(markup).not.toMatch(/text-muted-foreground\/\d+/u);
    expect(markup).toContain("Limited compatibility");
    expect(markup).toContain("text-xs");
  });

  it("renders the update-badge slot verbatim", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment({
      environment: { serverVersion: "0.4.2", capabilities: {} },
    });
    const markup = renderToStaticMarkup(
      <EnvironmentContextCard updateBadge={<span data-testid="update-badge">Up to date</span>} />,
    );
    expect(markup).toContain('data-testid="update-badge"');
  });

  it("wraps the long disconnect reason onto up to three lines instead of truncating it", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment(SERVER_CONFIG, RECONNECTING_NO_DATA);
    const host = mountCard();
    const status = statusLine(host, NO_DATA_STATUS);

    expect(status.className).toContain("line-clamp-3");
    expect(status.className).toContain("wrap-break-word");
    expect(status.className).not.toContain("truncate");
    expect(status.getAttribute("data-testid")).toBe("environment-context-card-status");
    // The clamp is visual only: the whole sentence stays in the line, so
    // assistive technology reads all of it.
    expect(accessibleText(status)).toBe(NO_DATA_STATUS);
    // The version stays visible, on the badge row below the reason, so the
    // clamp can never hide it.
    const version = statusLine(host, "Coddy Studio v0.6.2");
    expect(status.contains(version)).toBe(false);
    expect(version.closest(".flex-wrap")).not.toBeNull();
  });

  it.each([
    { ...RECONNECTING_NO_DATA, statusText: NO_DATA_STATUS },
    {
      phase: "error",
      error: "The remote host did not issue a pairing credential within 30 seconds.",
      statusText:
        "Connection failed. Reason: The remote host did not issue a pairing credential within 30 seconds.",
    },
  ])(
    "keeps the full $phase reason one hover or keyboard focus away",
    ({ phase, error, statusText }) => {
      h.reset();
      h.activeEnvironmentId = ENV_REMOTE;
      h.environment = remoteEnvironment(SERVER_CONFIG, { phase, error, traceId: null });
      const host = mountCard();
      const status = statusLine(host, statusText);

      // Hover (or keyboard focus) opens a tooltip holding the whole reason, and
      // the help cursor says so, as on the project list's reason line.
      expect([...host.querySelectorAll("[data-tooltip]")].map((node) => node.textContent)).toEqual([
        statusText,
      ]);
      expect(status.className).toContain("cursor-help");
      // Keyboard users reach the line with Tab, which opens the same tooltip.
      expect(status.tabIndex).toBe(0);
      act(() => status.focus());
      expect(document.activeElement).toBe(status);
      expect(accessibleText(status)).toContain(statusText);
    },
  );

  it("lets a short status wrap too, without a tooltip or an extra tab stop", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment(SERVER_CONFIG);
    const host = mountCard();
    const status = statusLine(host, "Connected");

    // At the narrowest sidebar the status wraps rather than clipping to "Co…".
    expect(status.className).toContain("line-clamp-3");
    expect(status.className).not.toContain("truncate");
    expect(accessibleText(status)).toBe("Connected Coddy Studio v0.6.2");
    // Nothing is hidden, so there is nothing to reveal.
    expect(status.hasAttribute("tabindex")).toBe(false);
    expect(status.className).not.toContain("cursor-help");
    expect(host.querySelector("[data-tooltip]")).toBeNull();
  });

  it("shows no reason line for a connected card that still carries a stale error", () => {
    h.reset();
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment(SERVER_CONFIG, {
      phase: "connected",
      error: "A stale failure from before.",
      traceId: null,
    });
    const host = mountCard();
    const status = statusLine(host, "Connected");

    // The status text leaves a connected card's error out, so the card shows
    // the short status with its version and no tooltip or extra tab stop.
    expect(accessibleText(status)).toBe("Connected Coddy Studio v0.6.2");
    expect(status.hasAttribute("tabindex")).toBe(false);
    expect(host.querySelector("[data-tooltip]")).toBeNull();
    expect(host.textContent).not.toContain("A stale failure from before.");
  });

  it("keeps the wrapped reason at the 12 px floor, in the solid secondary colour", () => {
    h.reset();
    h.compatVerdict = { kind: "legacy" };
    h.activeEnvironmentId = ENV_REMOTE;
    h.environment = remoteEnvironment(SERVER_CONFIG, RECONNECTING_NO_DATA);
    const host = mountCard();
    const status = statusLine(host, NO_DATA_STATUS);

    expect(status.closest(".text-xs")).not.toBeNull();
    expect(status.closest(".text-muted-foreground")).not.toBeNull();
    expect(host.innerHTML).not.toMatch(/text-\[(?:\d|1[01])px\]/u);
    // UI.md: no alpha-reduced text colour and no reduced opacity on text.
    expect(host.innerHTML).not.toMatch(/\btext-[a-z-]+\/\d+/u);
    expect(host.innerHTML).not.toMatch(/\bopacity-\d+/u);
  });
});
