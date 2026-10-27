// @vitest-environment happy-dom

import { type ReactNode, useSyncExternalStore } from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { EnvironmentId } from "@coddy/contracts";
import { DEFAULT_UNIFIED_SETTINGS, type UnifiedSettings } from "@coddy/contracts/settings";
import { SshConnectionTarget } from "@coddy/client-runtime/connection";
import type { PickHostFolderResult } from "../hostFolderPicker";
import type { EnvironmentPresentation } from "../../state/environments";

type Props = Record<string, unknown>;
type UpdateSettings = (patch: Partial<UnifiedSettings>) => Promise<unknown>;
type HostPicker = () => Promise<PickHostFolderResult>;

const harness = vi.hoisted(() => ({
  environments: [] as EnvironmentPresentation[],
  primaryEnvironment: null as EnvironmentPresentation | null,
  settingsByEnvironment: new Map<string, UnifiedSettings>(),
  settingsListeners: new Set<() => void>(),
  updateByEnvironment: new Map<string, UpdateSettings>(),
  hostPicker: vi.fn<HostPicker>(async () => ({
    _tag: "Selected" as const,
    environmentId: EnvironmentId.make("host-one"),
    path: "D:\\Worktrees",
  })),
  rows: [] as Props[],
  selects: [] as Props[],
  draftInputs: [] as Props[],
  buttons: [] as Props[],
  pickers: [] as Props[],
  reset() {
    this.environments = [];
    this.primaryEnvironment = null;
    this.settingsByEnvironment.clear();
    this.settingsListeners.clear();
    this.updateByEnvironment.clear();
    this.hostPicker.mockReset();
    this.hostPicker.mockResolvedValue({
      _tag: "Selected",
      environmentId: EnvironmentId.make("host-one"),
      path: "D:\\Worktrees",
    });
    this.rows.length = 0;
    this.selects.length = 0;
    this.draftInputs.length = 0;
    this.buttons.length = 0;
    this.pickers.length = 0;
  },
}));

vi.mock("../../state/environments", () => ({
  useEnvironments: () => ({ environments: harness.environments }),
  usePrimaryEnvironment: () => harness.primaryEnvironment,
}));

vi.mock("../../hooks/useSettings", () => ({
  useEnvironmentSettings: (environmentId: string) =>
    useSyncExternalStore(
      (listener) => {
        harness.settingsListeners.add(listener);
        return () => harness.settingsListeners.delete(listener);
      },
      () => harness.settingsByEnvironment.get(environmentId) ?? DEFAULT_UNIFIED_SETTINGS,
      () => harness.settingsByEnvironment.get(environmentId) ?? DEFAULT_UNIFIED_SETTINGS,
    ),
  useUpdateEnvironmentSettings: (environmentId: string) => {
    return async (patch: Partial<UnifiedSettings>) => {
      let update = harness.updateByEnvironment.get(environmentId);
      if (!update) {
        update = vi.fn(async (nextPatch: Partial<UnifiedSettings>) => ({
          _tag: "Success",
          value: { ...DEFAULT_UNIFIED_SETTINGS, ...nextPatch },
        }));
        harness.updateByEnvironment.set(environmentId, update);
      }
      return update(patch);
    };
  },
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: { readonly _tag?: string }) => result._tag === "Interrupted",
  squashAtomCommandFailure: (result: { readonly error?: unknown }) => result.error,
}));

vi.mock("./settingsLayout", () => ({
  SettingsRow: (props: Props) => {
    harness.rows.push(props);
    return (
      <section>
        {props.title as ReactNode}
        {props.description as ReactNode}
        {props.status as ReactNode}
        {props.resetAction as ReactNode}
        {props.control as ReactNode}
      </section>
    );
  },
  SettingResetButton: (props: Props) => {
    harness.buttons.push({ ...props, children: "Reset" });
    return <button aria-label={`Reset ${String(props.label)} to default`} />;
  },
}));

vi.mock("../ui/button", () => ({
  Button: (props: Props) => {
    harness.buttons.push(props);
    return <button disabled={Boolean(props.disabled)}>{props.children as ReactNode}</button>;
  },
}));

vi.mock("../ui/draft-input", () => ({
  DraftInput: (props: Props) => {
    harness.draftInputs.push(props);
    return <input aria-label={String(props["aria-label"])} value={String(props.value)} readOnly />;
  },
}));

vi.mock("../ui/select", () => ({
  Select: (props: Props) => {
    harness.selects.push(props);
    return <div>{props.children as ReactNode}</div>;
  },
  SelectTrigger: (props: Props) => (
    <button aria-label={String(props["aria-label"])}>{props.children as ReactNode}</button>
  ),
  SelectValue: (props: Props) => <>{props.children as ReactNode}</>,
  SelectPopup: (props: Props) => <>{props.children as ReactNode}</>,
  SelectItem: (props: Props) => <>{props.children as ReactNode}</>,
}));

vi.mock("./RemoteDirectoryPickerDialog", () => ({
  RemoteDirectoryPickerDialog: (props: Props) => {
    harness.pickers.push(props);
    return props.open ? <div data-testid="remote-directory-picker" /> : null;
  },
}));

vi.mock("../hostFolderPicker", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../hostFolderPicker")>();
  return { ...actual, pickHostFolder: harness.hostPicker };
});

const { WorktreeWorkspaceSetting } = await import("./WorktreeWorkspaceSetting");

function connectedEnvironment(id: string, label: string): EnvironmentPresentation {
  return {
    environmentId: EnvironmentId.make(id),
    label,
    displayUrl: null,
    relayManaged: false,
    entry: {
      target: { _tag: "PrimaryConnectionTarget" },
    } as EnvironmentPresentation["entry"],
    serverConfig: null,
    connection: { phase: "connected", error: null, traceId: null },
  };
}

function disconnectedEnvironment(id: string, label: string): EnvironmentPresentation {
  return {
    ...connectedEnvironment(id, label),
    connection: { phase: "offline", error: null, traceId: null },
  };
}

interface MountedSetting {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mounted: MountedSetting[] = [];

async function renderSetting(): Promise<MountedSetting> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const setting = { container, root };
  mounted.push(setting);
  await act(async () => root.render(<WorktreeWorkspaceSetting />));
  return setting;
}

async function rerender(setting: MountedSetting): Promise<void> {
  await act(async () => {
    for (const listener of harness.settingsListeners) listener();
    setting.root.render(<WorktreeWorkspaceSetting />);
  });
}

function latest<T>(items: T[]): T {
  const item = items.at(-1);
  if (!item) throw new Error("Expected captured props");
  return item;
}

function select(label: string): Props {
  const hasAccessibleLabel = (node: unknown): boolean => {
    if (Array.isArray(node)) return node.some(hasAccessibleLabel);
    if (node !== null && typeof node === "object" && "props" in node) {
      const props = (node as { props: Props }).props;
      return props["aria-label"] === label || hasAccessibleLabel(props.children);
    }
    return false;
  };
  const item = harness.selects.findLast((candidate) => hasAccessibleLabel(candidate.children));
  if (!item) throw new Error(`No select labelled ${label}`);
  return item;
}

async function invoke(props: Props, callback: string, ...args: unknown[]): Promise<void> {
  const handler = props[callback];
  if (typeof handler !== "function") throw new Error(`Missing ${callback}`);
  await act(async () => {
    await handler(...args);
  });
}

async function commitDraft(label: string, value: string): Promise<void> {
  const input = latest(
    harness.draftInputs.filter((candidate) => candidate["aria-label"] === label),
  );
  await invoke(input, "onCommit", value);
}

function button(label: string): Props {
  const item = harness.buttons.find((candidate) => candidate.children === label);
  if (!item) throw new Error(`No button labelled ${label}`);
  return item;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  Object.defineProperty(window, "desktopBridge", { configurable: true, value: {} });
  harness.reset();
});

afterEach(async () => {
  for (const setting of mounted.splice(0)) {
    await act(async () => setting.root.unmount());
    setting.container.remove();
  }
  Reflect.deleteProperty(window, "desktopBridge");
});

describe("WorktreeWorkspaceSetting", () => {
  it("shows the default copy and routes manual commits to the only host", async () => {
    harness.environments = [connectedEnvironment("host-one", "Local")];
    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "",
    });
    const setting = await renderSetting();

    expect(setting.container.textContent).toContain("Workspace");
    expect(setting.container.textContent).toContain(
      "Default: worktrees are stored next to each project.",
    );
    expect(harness.selects).toHaveLength(0);
    await commitDraft("Workspace directory", "D:\\Worktrees");
    expect(harness.updateByEnvironment.get("host-one")).toHaveBeenCalledWith({
      worktreeBaseDirectory: "D:\\Worktrees",
    });
  });

  it("initializes and routes Workspace to the non-first primary host", async () => {
    const local = connectedEnvironment("host-one", "Local");
    const buildServer = connectedEnvironment("host-two", "Build server");
    harness.environments = [local, buildServer];
    harness.primaryEnvironment = buildServer;
    harness.settingsByEnvironment.set("host-two", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "/srv/worktrees",
    });

    await renderSetting();

    expect(select("Workspace host").value).toBe("host-two");
    expect(latest(harness.draftInputs).value).toBe("/srv/worktrees");
    await commitDraft("Workspace directory", "/srv/next-worktrees");
    expect(harness.updateByEnvironment.get("host-two")).toHaveBeenCalledWith({
      worktreeBaseDirectory: "/srv/next-worktrees",
    });
    expect(harness.updateByEnvironment.get("host-one")).toBeUndefined();
  });

  it("shows Host for multiple connected servers and remounts the selected editor", async () => {
    harness.environments = [
      connectedEnvironment("host-one", "Local"),
      connectedEnvironment("host-two", "Build server"),
    ];
    const setting = await renderSetting();

    expect(select("Workspace host").value).toBe("host-one");
    await invoke(select("Workspace host"), "onValueChange", "host-two");
    await rerender(setting);
    expect(select("Workspace host").value).toBe("host-two");
  });

  it("uses the shared native picker for the primary desktop host", async () => {
    const local = connectedEnvironment("host-one", "This device");
    harness.environments = [local];
    harness.primaryEnvironment = local;
    await renderSetting();

    await invoke(button("Browse"), "onClick");
    expect(harness.hostPicker).toHaveBeenCalledOnce();
    expect(latest(harness.pickers).open).toBe(false);
    expect(harness.updateByEnvironment.get("host-one")).toHaveBeenCalledWith({
      worktreeBaseDirectory: "D:\\Worktrees",
    });
  });

  it("keeps the server browser for a remote host", async () => {
    harness.environments = [
      {
        ...connectedEnvironment("remote", "SSH host"),
        entry: {
          target: new SshConnectionTarget({
            environmentId: EnvironmentId.make("remote"),
            label: "SSH host",
            connectionId: "ssh:remote",
          }),
        } as EnvironmentPresentation["entry"],
      },
    ];
    harness.primaryEnvironment = null;
    await renderSetting();

    await invoke(button("Browse"), "onClick");

    expect(harness.hostPicker).not.toHaveBeenCalled();
    expect(latest(harness.pickers).open).toBe(true);
  });

  it("leaves Workspace unchanged when native picking is cancelled", async () => {
    const local = connectedEnvironment("host-one", "This device");
    harness.environments = [local];
    harness.primaryEnvironment = local;
    harness.hostPicker.mockResolvedValueOnce({ _tag: "Cancelled" });
    await renderSetting();

    await invoke(button("Browse"), "onClick");

    expect(harness.updateByEnvironment.get("host-one")).toBeUndefined();
  });

  it("shows a native picker failure without changing Workspace", async () => {
    const local = connectedEnvironment("host-one", "This device");
    harness.environments = [local];
    harness.primaryEnvironment = local;
    harness.hostPicker.mockResolvedValueOnce({
      _tag: "Failure",
      message: "Native folder picker failed.",
    });
    const setting = await renderSetting();

    await invoke(button("Browse"), "onClick");

    expect(setting.container.textContent).toContain("Native folder picker failed.");
    expect(harness.updateByEnvironment.get("host-one")).toBeUndefined();
  });

  it("ignores a native selection after the selected host changes", async () => {
    const local = connectedEnvironment("host-one", "This device");
    const remote = connectedEnvironment("host-two", "SSH host");
    harness.environments = [local, remote];
    harness.primaryEnvironment = local;
    let resolveSelection!: (result: PickHostFolderResult) => void;
    harness.hostPicker.mockReturnValueOnce(
      new Promise<PickHostFolderResult>((resolve) => {
        resolveSelection = resolve;
      }),
    );
    const setting = await renderSetting();

    await act(async () => {
      void (button("Browse").onClick as () => Promise<void>)();
    });
    await invoke(select("Workspace host"), "onValueChange", "host-two");
    await rerender(setting);
    await act(async () => {
      resolveSelection({
        _tag: "Selected",
        environmentId: EnvironmentId.make("host-one"),
        path: "D:\\Stale",
      });
      await Promise.resolve();
    });

    expect(harness.updateByEnvironment.get("host-one")).toBeUndefined();
    expect(harness.updateByEnvironment.get("host-two")).toBeUndefined();
  });

  it("shows the canonical Workspace returned by the server before the settings stream catches up", async () => {
    harness.environments = [connectedEnvironment("host-one", "Local")];
    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "",
    });
    let resolveUpdate:
      | ((result: { readonly _tag: "Success"; readonly value: UnifiedSettings }) => void)
      | undefined;
    harness.updateByEnvironment.set(
      "host-one",
      vi.fn(
        () =>
          new Promise<{ readonly _tag: "Success"; readonly value: UnifiedSettings }>((resolve) => {
            resolveUpdate = resolve;
          }),
      ),
    );
    const setting = await renderSetting();

    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "",
    });
    await rerender(setting);

    await invoke(button("Browse"), "onClick");
    await invoke(latest(harness.pickers), "onSelect", "C:\\Users\\mauro\\WORKTR~1");

    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "",
    });
    await rerender(setting);
    await act(async () => {
      resolveUpdate?.({
        _tag: "Success",
        value: {
          ...DEFAULT_UNIFIED_SETTINGS,
          worktreeBaseDirectory: "C:\\Users\\mauro\\Worktrees",
        },
      });
      await Promise.resolve();
    });

    expect(latest(harness.draftInputs).value).toBe("C:\\Users\\mauro\\Worktrees");

    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "",
    });
    await rerender(setting);

    expect(latest(harness.draftInputs).value).toBe("");
  });

  it("resets a configured workspace", async () => {
    harness.environments = [connectedEnvironment("host-one", "Local")];
    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "/srv/worktrees",
    });
    await renderSetting();

    await invoke(button("Reset"), "onClick", { stopPropagation: vi.fn() });
    expect(harness.updateByEnvironment.get("host-one")).toHaveBeenCalledWith({
      worktreeBaseDirectory: "",
    });
  });

  it("shows a typed failure while retaining the server-owned configured directory", async () => {
    harness.environments = [connectedEnvironment("host-one", "Local")];
    harness.settingsByEnvironment.set("host-one", {
      ...DEFAULT_UNIFIED_SETTINGS,
      worktreeBaseDirectory: "/srv/old",
    });
    harness.updateByEnvironment.set(
      "host-one",
      vi.fn(async () => ({ _tag: "Failure", error: new Error("Permission denied") })),
    );
    const setting = await renderSetting();

    await commitDraft("Workspace directory", "/srv/new");
    expect(setting.container.textContent).toContain("Permission denied");
    expect(latest(harness.draftInputs).value).toBe("/srv/old");
  });

  it("disables Workspace editing and Browse for a disconnected selected host", async () => {
    harness.environments = [disconnectedEnvironment("host-one", "Offline host")];
    const setting = await renderSetting();

    expect(setting.container.textContent).toContain("Reconnect Offline host to change Workspace.");
    expect(latest(harness.draftInputs).disabled).toBe(true);
    expect(button("Browse").disabled).toBe(true);
  });

  it("closes the previous picker lifecycle when the host changes", async () => {
    harness.environments = [
      connectedEnvironment("host-one", "Local"),
      connectedEnvironment("host-two", "Build server"),
    ];
    const setting = await renderSetting();
    await invoke(button("Browse"), "onClick");
    const priorPicker = latest(harness.pickers);

    await invoke(select("Workspace host"), "onValueChange", "host-two");
    await rerender(setting);
    expect(latest(harness.pickers).environmentId).toBe("host-two");
    expect(latest(harness.pickers).open).toBe(false);

    await invoke(priorPicker, "onSelect", "/stale/worktrees");
    expect(harness.updateByEnvironment.get("host-two")).toBeUndefined();
  });
});
