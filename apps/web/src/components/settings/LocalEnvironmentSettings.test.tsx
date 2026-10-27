import { act, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import type { DesktopWslState } from "@coddy/contracts";

type AnyProps = Record<string, unknown>;

interface CapturedControl {
  readonly kind: string;
  readonly label: string;
  readonly props: AnyProps;
}

interface DesktopBridgeStub {
  readonly setWslBackendEnabled: ReturnType<typeof vi.fn>;
  readonly setWslDistro: ReturnType<typeof vi.fn>;
  readonly setWslOnly: ReturnType<typeof vi.fn>;
}

const mountedTrees: Array<{ readonly container: HTMLDivElement; readonly root: Root }> = [];
let domWindow: Window | null = null;

const h = vi.hoisted(() => ({
  controls: [] as CapturedControl[],
  environments: [] as unknown[],
  wslQuery: {
    data: null as DesktopWslState | null,
    error: null as string | null,
    isPending: false,
  },
  desktopWslAtom: Symbol("desktopWslStateAtom"),
  refreshDesktopWslState: vi.fn(),
  toastAdd: vi.fn(),
  textOf: (node: unknown): string => {
    if (typeof node === "string" || typeof node === "number") return String(node);
    if (Array.isArray(node)) return node.map(h.textOf).join("");
    if (node !== null && typeof node === "object" && "props" in node) {
      return h.textOf((node as { props: { children?: unknown } }).props.children);
    }
    return "";
  },
}));

vi.mock("@tanstack/react-router", () => ({
  Link: (props: AnyProps) => <a>{props.children as ReactNode}</a>,
}));

vi.mock("~/connection/desktopLocal", () => ({
  isDesktopLocalConnectionTarget: (target: unknown) =>
    (target as { _tag?: string })._tag === "DesktopLocalConnectionTarget",
}));

vi.mock("~/state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    if (atom !== h.desktopWslAtom) {
      return { data: null, error: null, isPending: false };
    }
    return h.wslQuery;
  },
}));

vi.mock("~/state/desktopWslState", () => ({
  desktopWslStateAtom: h.desktopWslAtom,
  refreshDesktopWslState: h.refreshDesktopWslState,
}));

vi.mock("./settingsLayout", () => ({
  SettingsPageContainer: (props: AnyProps) => (
    <div data-testid="settings-page">{props.children as ReactNode}</div>
  ),
  SettingsSection: (props: AnyProps) => (
    <section>
      <h2>{props.title as ReactNode}</h2>
      {props.children as ReactNode}
    </section>
  ),
  SettingsRow: (props: AnyProps) => (
    <div data-testid="settings-row">
      {props.title as ReactNode}
      {props.description as ReactNode}
      {props.status as ReactNode}
      {props.control as ReactNode}
      {props.children as ReactNode}
    </div>
  ),
}));

vi.mock("../ui/button", () => ({
  Button: (props: AnyProps) => {
    h.controls.push({
      kind: "button",
      label: (props["aria-label"] as string | undefined) ?? h.textOf(props.children),
      props,
    });
    return <button disabled={Boolean(props.disabled)}>{props.children as ReactNode}</button>;
  },
}));

vi.mock("../ui/select", () => ({
  Select: (props: AnyProps) => {
    h.controls.push({ kind: "select", label: String(props.value), props });
    return <div data-value={String(props.value)}>{props.children as ReactNode}</div>;
  },
  SelectTrigger: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  SelectValue: (props: AnyProps) => <span>{props.children as ReactNode}</span>,
  SelectPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  SelectItem: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
}));

vi.mock("../ui/switch", () => ({
  Switch: (props: AnyProps) => {
    h.controls.push({
      kind: "switch",
      label: (props["aria-label"] as string | undefined) ?? "",
      props,
    });
    return <span role="switch" aria-label={props["aria-label"] as string | undefined} />;
  },
}));

vi.mock("../ui/alert-dialog", () => ({
  AlertDialog: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogHeader: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogTitle: (props: AnyProps) => <h3>{props.children as ReactNode}</h3>,
  AlertDialogDescription: (props: AnyProps) => <p>{props.children as ReactNode}</p>,
  AlertDialogFooter: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogClose: (props: AnyProps) => <>{props.children as ReactNode}</>,
}));

vi.mock("../ui/spinner", () => ({
  Spinner: () => <span data-spinner />,
}));

vi.mock("../ui/toast", () => ({
  toastManager: { add: h.toastAdd },
  stackedThreadToast: (options: unknown) => options,
}));

import { LocalEnvironmentSettings } from "./LocalEnvironmentSettings";

function wslState(overrides: Partial<DesktopWslState> = {}): DesktopWslState {
  return {
    enabled: true,
    distro: "Ubuntu",
    available: true,
    wslOnly: false,
    distros: [
      { name: "Ubuntu", isDefault: true, version: 2 },
      { name: "Debian", isDefault: false, version: 2 },
    ],
    preflightError: null,
    ...overrides,
  };
}

function createDesktopBridgeStub(): DesktopBridgeStub {
  const state = wslState();
  return {
    setWslBackendEnabled: vi.fn(async () => state),
    setWslDistro: vi.fn(async () => state),
    setWslOnly: vi.fn(async () => state),
  };
}

function installDesktopBridge(): DesktopBridgeStub {
  const bridge = createDesktopBridgeStub();
  Object.defineProperty(window, "desktopBridge", { configurable: true, value: bridge });
  return bridge;
}

function findControls(kind: string, label: string): CapturedControl[] {
  const exact = h.controls.filter((entry) => entry.kind === kind && entry.label === label);
  return exact.length > 0
    ? exact
    : h.controls.filter((entry) => entry.kind === kind && entry.label.includes(label));
}

function control(kind: string, label: string): CapturedControl {
  const found = findControls(kind, label).at(-1);
  if (!found) throw new Error(`No ${kind} control labelled ${label}`);
  return found;
}

function invoke(entry: CapturedControl, handlerName: string, ...args: unknown[]): unknown {
  const handler = entry.props[handlerName];
  if (typeof handler !== "function") {
    throw new Error(`Control ${entry.label} has no handler ${handlerName}`);
  }
  return (handler as (...input: unknown[]) => unknown)(...args);
}

async function mount(node: ReactElement = <LocalEnvironmentSettings />): Promise<HTMLDivElement> {
  h.controls.length = 0;
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedTrees.push({ container, root });
  await act(async () => root.render(node));
  return container;
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  domWindow = new Window({ url: "https://desktop.localhost/settings/local-environment" });
  vi.stubGlobal("window", domWindow);
  vi.stubGlobal("document", domWindow.document);
  vi.stubGlobal("navigator", domWindow.navigator);
  vi.stubGlobal("Node", domWindow.Node);
  vi.stubGlobal("Element", domWindow.Element);
  vi.stubGlobal("HTMLElement", domWindow.HTMLElement);
  vi.stubGlobal("Event", domWindow.Event);
  vi.stubGlobal("MouseEvent", domWindow.MouseEvent);
  vi.stubGlobal("MutationObserver", domWindow.MutationObserver);
  vi.stubGlobal("ResizeObserver", domWindow.ResizeObserver);
  vi.stubGlobal("getComputedStyle", domWindow.getComputedStyle.bind(domWindow));
  vi.stubGlobal("requestAnimationFrame", domWindow.requestAnimationFrame.bind(domWindow));
  vi.stubGlobal("cancelAnimationFrame", domWindow.cancelAnimationFrame.bind(domWindow));
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  h.controls.length = 0;
  h.environments = [];
  h.wslQuery = { data: wslState(), error: null, isPending: false };
  h.refreshDesktopWslState.mockReset();
  h.toastAdd.mockReset();
});

afterEach(async () => {
  for (const { root, container } of mountedTrees.splice(0)) {
    await act(async () => root.unmount());
    container.remove();
  }
  domWindow?.close();
  domWindow = null;
  vi.unstubAllGlobals();
});

describe("LocalEnvironmentSettings", () => {
  it("renders an accessible unavailable row when the desktop bridge is missing", async () => {
    const container = await mount();

    const alert = container.querySelector('[role="alert"]');
    expect(alert).not.toBeNull();
    expect(alert!.textContent).toContain("Desktop bridge unavailable");
    expect(container.textContent).toContain("WSL backend unavailable");
    expect(container.textContent).toContain(
      "Desktop integration is unavailable. Restart Coddy Studio to manage the local WSL backend.",
    );
    expect(container.textContent).not.toContain("Add environment");
  });

  it("renders an accessible loading row while WSL state is pending", async () => {
    installDesktopBridge();
    h.wslQuery = { data: null, error: null, isPending: true };
    const container = await mount();

    const status = container.querySelector('[role="status"]');
    expect(status).not.toBeNull();
    expect(status!.textContent).toContain("Loading WSL backend settings");
    expect(container.textContent).toContain("WSL backend");
    expect(findControls("button", "Retry")).toHaveLength(0);
  });

  it("renders a retryable accessible unavailable row when no WSL state is returned", async () => {
    installDesktopBridge();
    h.wslQuery = { data: null, error: null, isPending: false };
    const container = await mount();

    const alert = container.querySelector('[role="alert"]');
    expect(alert).not.toBeNull();
    expect(alert!.textContent).toContain("WSL backend state unavailable");
    expect(container.textContent).toContain("Couldn't load the WSL backend state.");
    await act(async () => invoke(control("button", "Retry"), "onClick"));
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(1);
  });

  it("keeps an unavailable disabled WSL backend discoverable and retryable", async () => {
    const bridge = installDesktopBridge();
    h.wslQuery.data = wslState({
      enabled: false,
      distro: null,
      available: false,
      wslOnly: false,
      distros: [],
    });
    const container = await mount();

    const alert = container.querySelector('[role="alert"]');
    expect(alert).not.toBeNull();
    expect(alert!.textContent).toContain("WSL backend unavailable");
    expect(container.textContent).toContain("WSL backend");

    await act(async () => invoke(control("button", "Retry"), "onClick"));
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(1);
    expect(bridge.setWslBackendEnabled).not.toHaveBeenCalled();
    expect(bridge.setWslDistro).not.toHaveBeenCalled();
    expect(bridge.setWslOnly).not.toHaveBeenCalled();

    for (const hiddenText of [
      "Network access",
      "Tailscale HTTPS",
      "Authorized clients",
      "Coddy Connect",
      "Remote environments",
      "Add environment",
      "SSH",
    ]) {
      expect(container.textContent).not.toContain(hiddenText);
    }
  });

  it("renders only Windows-local WSL settings and no remote controls", async () => {
    installDesktopBridge();
    h.environments = [{ entry: { target: { _tag: "DesktopLocalConnectionTarget" } } }];
    const container = await mount();
    let markup = container.innerHTML;

    expect(markup).toContain("Local environment");
    expect(markup).toContain("WSL backend");
    expect(markup).toContain("Ubuntu");
    expect(markup).toContain("Debian");
    expect(markup).toContain("WSL only");

    await act(async () => invoke(control("switch", "Run WSL only"), "onCheckedChange", true));
    markup = container.innerHTML;
    expect(markup).toContain("Run only the WSL backend?");
    await act(async () => invoke(control("select", "Ubuntu"), "onValueChange", "backend:wsl-off"));
    markup = container.innerHTML;
    expect(markup).toContain("Disable WSL backend?");
    for (const hiddenText of [
      "Network access",
      "Tailscale HTTPS",
      "Authorized clients",
      "Coddy Connect",
      "Remote environments",
      "Add environment",
      "SSH",
    ]) {
      expect(markup).not.toContain(hiddenText);
    }
  });

  it("applies direct disable and distro changes, refreshes, and reports failures", async () => {
    const bridge = installDesktopBridge();
    h.environments = [];
    const container = await mount();
    const select = control("select", "Ubuntu");

    await act(async () => {
      invoke(select, "onValueChange", "backend:wsl-off");
      await flush();
    });
    expect(bridge.setWslBackendEnabled).toHaveBeenCalledTimes(1);
    expect(bridge.setWslBackendEnabled).toHaveBeenCalledWith(false);
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(1);

    await act(async () => {
      invoke(select, "onValueChange", "Debian");
      await flush();
    });
    expect(bridge.setWslDistro).toHaveBeenCalledWith("Debian");

    bridge.setWslDistro.mockRejectedValueOnce(new Error("distro switch failed"));
    await act(async () => {
      invoke(select, "onValueChange", "Debian");
      await flush();
    });
    expect(container.innerHTML).toContain("distro switch failed");
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not change WSL backend" }),
    );
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(3);
  });

  it("confirms a registered WSL distro switch before mutating", async () => {
    const bridge = installDesktopBridge();
    h.environments = [{ entry: { target: { _tag: "DesktopLocalConnectionTarget" } } }];
    await mount();

    await act(async () => {
      invoke(control("select", "Ubuntu"), "onValueChange", 42);
      invoke(control("select", "Ubuntu"), "onValueChange", "Ubuntu");
    });
    expect(bridge.setWslDistro).not.toHaveBeenCalled();

    await act(async () => invoke(control("select", "Ubuntu"), "onValueChange", "Debian"));
    expect(bridge.setWslDistro).not.toHaveBeenCalled();
    expect(control("button", "Switch distro")).toBeDefined();

    await act(async () => {
      invoke(control("button", "Switch distro"), "onClick");
      await flush();
    });
    expect(bridge.setWslDistro).toHaveBeenCalledTimes(1);
    expect(bridge.setWslDistro).toHaveBeenCalledWith("Debian");
  });

  it("confirms WSL-only changes and applies the selected value", async () => {
    const bridge = installDesktopBridge();
    await mount();

    await act(async () => {
      invoke(control("switch", "Run WSL only"), "onCheckedChange", false);
      invoke(control("switch", "Run WSL only"), "onCheckedChange", true);
    });
    expect(bridge.setWslOnly).not.toHaveBeenCalled();
    expect(control("button", "Restart and enable")).toBeDefined();

    await act(async () => {
      invoke(control("button", "Restart and enable"), "onClick");
      await flush();
    });
    expect(bridge.setWslOnly).toHaveBeenCalledTimes(1);
    expect(bridge.setWslOnly).toHaveBeenCalledWith(true);
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(1);
  });

  it("uses one atomic bridge command for confirmed WSL-only disable", async () => {
    const bridge = installDesktopBridge();
    h.environments = [{ entry: { target: { _tag: "DesktopLocalConnectionTarget" } } }];
    h.wslQuery.data = wslState({ wslOnly: true });
    await mount();

    await act(async () => invoke(control("select", "Ubuntu"), "onValueChange", "backend:wsl-off"));
    expect(bridge.setWslBackendEnabled).not.toHaveBeenCalled();

    await act(async () => {
      invoke(control("button", "Switch to Windows"), "onClick");
      await flush();
    });
    expect(bridge.setWslBackendEnabled).toHaveBeenCalledTimes(1);
    expect(bridge.setWslBackendEnabled).toHaveBeenCalledWith(false);
    expect(bridge.setWslOnly).not.toHaveBeenCalled();
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(1);
  });

  it("offers both enable modes while WSL is off", async () => {
    const bridge = installDesktopBridge();
    h.wslQuery.data = wslState({ enabled: false, distro: null, distros: [] });
    await mount();

    await act(async () =>
      invoke(control("select", "backend:wsl-off"), "onValueChange", "backend:default-wsl"),
    );
    expect(bridge.setWslBackendEnabled).not.toHaveBeenCalled();
    expect(control("button", "Use only WSL")).toBeDefined();
    expect(control("button", "Run both backends")).toBeDefined();
    expect(findControls("switch", "Run WSL only")).toHaveLength(0);
  });

  it("keeps load and WSL-only startup failures retryable without fallback claims", async () => {
    const bridge = installDesktopBridge();
    h.wslQuery = { data: null, error: "wsl state failed to load", isPending: false };
    let container = await mount();
    expect(container.innerHTML).toContain("Couldn't load the WSL backend state.");
    await act(async () => invoke(control("button", "Retry"), "onClick"));
    expect(h.refreshDesktopWslState).toHaveBeenCalledTimes(1);

    h.wslQuery = {
      data: wslState({
        wslOnly: true,
        preflightError: {
          kind: "wsl-primary-unavailable",
          detail: "the selected distribution cannot start",
        },
      }),
      error: null,
      isPending: false,
    };
    container = await mount();
    expect(container.innerHTML).toContain("no Windows backend was substituted");
    expect(container.innerHTML).toContain("the selected distribution cannot start");

    await act(async () => {
      invoke(control("button", "Retry WSL"), "onClick");
      await flush();
    });
    expect(bridge.setWslDistro).toHaveBeenCalledWith("Ubuntu");

    await act(async () => {
      invoke(control("button", "Switch to Windows"), "onClick");
      await flush();
    });
    expect(bridge.setWslBackendEnabled).toHaveBeenCalledWith(false);
    expect(bridge.setWslOnly).not.toHaveBeenCalled();
  });

  it("keeps secondary WSL failure distinct and allows turning it off", async () => {
    const bridge = installDesktopBridge();
    h.wslQuery.data = wslState({
      preflightError: {
        kind: "wsl-secondary-unavailable",
        detail: "the configured WSL secondary could not start",
      },
    });
    const container = await mount();

    expect(container.innerHTML).toContain("Windows backend remains primary");
    expect(container.innerHTML).not.toContain("no Windows backend was substituted");
    expect(findControls("button", "Switch to Windows")).toHaveLength(0);

    await act(async () => {
      invoke(control("button", "Turn off WSL"), "onClick");
      await flush();
    });
    expect(bridge.setWslBackendEnabled).toHaveBeenCalledWith(false);
    expect(bridge.setWslOnly).not.toHaveBeenCalled();
  });
});
