import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  environments: [] as Array<{ entry: { target: Record<string, unknown> } }>,
  groups: [] as Array<Record<string, unknown>>,
  updateGroups: [] as Array<{ candidates: unknown[] }>,
  isAnySettling: false,
  notificationKey: null as string | null,
  dismissedKeys: new Set<string>(),
  dismissKey: vi.fn(),
  effects: [] as Array<() => void | (() => void)>,
  refSeeds: [] as unknown[],
  refs: [] as Array<{ current: unknown }>,
  refIndex: 0,
  settleElapsed: false,
  setSettleElapsed: vi.fn(),
  navigate: vi.fn(),
  toastAdd: vi.fn((_config: unknown) => "toast-1"),
  toastClose: vi.fn(),
  stackedToast: vi.fn((value: unknown) => value),
  rowProps: [] as Array<Record<string, unknown>>,
  collectCandidates: vi.fn((candidates: unknown[]) => candidates),
}));

vi.mock("react", async (importOriginal) => ({
  ...(await importOriginal<typeof import("react")>()),
  useCallback: (callback: unknown) => callback,
  useEffect: (effect: () => void | (() => void)) => harness.effects.push(effect),
  useMemo: (factory: () => unknown) => factory(),
  useRef: (initial: unknown) => {
    const index = harness.refIndex++;
    const ref = { current: index < harness.refSeeds.length ? harness.refSeeds[index] : initial };
    harness.refs[index] = ref;
    return ref;
  },
  useState: () => [harness.settleElapsed, harness.setSettleElapsed],
}));
vi.mock("@tanstack/react-router", () => ({ useNavigate: () => harness.navigate }));
vi.mock("~/state/environments", () => ({
  useEnvironments: () => ({ environments: harness.environments }),
}));
vi.mock("~/connection/desktopLocal", () => ({
  isDesktopLocalConnectionTarget: (target: { local?: boolean }) => target.local === true,
}));
vi.mock("../providerUpdateDismissal", () => ({
  useDismissedProviderUpdateNotificationKeys: () => ({
    dismissedNotificationKeys: harness.dismissedKeys,
    dismissNotificationKey: harness.dismissKey,
  }),
}));
vi.mock("./ProviderUpdateLaunchNotification.environments", () => ({
  useLocalEnvironmentUpdateGroups: () => ({
    groups: harness.groups,
    isAnySettling: harness.isAnySettling,
  }),
}));
vi.mock("./ProviderUpdateLaunchNotification.logic", () => ({
  collectProviderUpdateCandidates: (candidates: unknown[]) => harness.collectCandidates(candidates),
  environmentGroupsWithUpdates: () => harness.updateGroups,
  getProviderUpdateInitialToastView: () => ({ title: "Provider updates available" }),
  localEnvironmentUpdateNotificationKey: () => harness.notificationKey,
}));
vi.mock("./ProviderUpdatePrimaryNotification", () => ({
  ProviderUpdatePrimaryNotification: () => <div data-primary-notification />,
}));
vi.mock("./ProviderUpdateEnvironmentRows", () => ({
  ProviderUpdateEnvironmentRows: (props: Record<string, unknown>) => {
    harness.rowProps.push(props);
    return <div data-environment-rows />;
  },
}));
vi.mock("./ui/toast", () => ({
  toastManager: { add: harness.toastAdd, close: harness.toastClose },
  stackedThreadToast: harness.stackedToast,
}));

import { ProviderUpdateLaunchNotification } from "./ProviderUpdateLaunchNotification";

function prepareRender(): void {
  harness.effects.length = 0;
  harness.refs.length = 0;
  harness.refIndex = 0;
  harness.rowProps.length = 0;
}

function renderNotification(): string {
  prepareRender();
  return renderToStaticMarkup(<ProviderUpdateLaunchNotification />);
}

function toastConfig(): Record<string, unknown> {
  const config = harness.toastAdd.mock.calls.at(-1)?.[0];
  if (!config || typeof config !== "object") throw new Error("Missing toast configuration");
  return config as Record<string, unknown>;
}

beforeEach(() => {
  harness.environments = [];
  harness.groups = [];
  harness.updateGroups = [];
  harness.isAnySettling = false;
  harness.notificationKey = null;
  harness.dismissedKeys = new Set();
  harness.dismissKey.mockReset();
  harness.refSeeds = [];
  harness.settleElapsed = false;
  harness.setSettleElapsed.mockReset();
  harness.navigate.mockReset();
  harness.toastAdd.mockReset();
  harness.toastAdd.mockReturnValue("toast-1");
  harness.toastClose.mockReset();
  harness.stackedToast.mockClear();
  harness.collectCandidates.mockClear();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("ProviderUpdateLaunchNotification", () => {
  it("uses the primary flow unless a desktop-local secondary exists", () => {
    harness.environments = [
      { entry: { target: { local: false } } },
      { entry: { target: { local: false } } },
    ];
    expect(renderNotification()).toContain("data-primary-notification");

    harness.environments.push({ entry: { target: { local: true } } });
    expect(renderNotification()).toBe("");
  });

  it("tracks notification keys and settles immediately when backends are ready", () => {
    harness.environments = [{ entry: { target: { local: true } } }];
    harness.notificationKey = null;
    harness.settleElapsed = true;
    renderNotification();
    expect(harness.setSettleElapsed).toHaveBeenCalledWith(false);
    harness.effects[1]?.();
    expect(harness.refs[1]?.current).toBeNull();
    expect(harness.effects[2]?.()).toBeUndefined();
  });

  it("waits up to thirty seconds for settling backends", () => {
    vi.useFakeTimers();
    harness.environments = [{ entry: { target: { local: true } } }];
    harness.isAnySettling = true;
    renderNotification();
    const cleanup = harness.effects[2]?.();
    vi.advanceTimersByTime(30_000);
    expect(harness.setSettleElapsed).toHaveBeenCalledWith(true);
    if (typeof cleanup === "function") cleanup();
  });

  it("opens a toast, records interaction, navigates, and dismisses the live key", () => {
    harness.environments = [{ entry: { target: { local: true } } }];
    harness.notificationKey = "updates-success";
    harness.updateGroups = [{ candidates: [{ provider: "codex" }] }];
    renderNotification();
    harness.effects[1]?.();
    harness.effects[2]?.();
    harness.effects[3]?.();
    expect(harness.toastAdd).toHaveBeenCalledOnce();
    expect(harness.refs[0]?.current).toEqual({ toastId: "toast-1", key: "updates-success" });
    expect(harness.collectCandidates).toHaveBeenCalledWith([{ provider: "codex" }]);

    const config = toastConfig();
    renderToStaticMarkup(config.description as React.ReactElement);
    const onInteract = harness.rowProps[0]?.onInteract;
    if (typeof onInteract !== "function") throw new Error("Missing interaction handler");
    onInteract();
    expect(harness.refs[2]?.current).toBe(true);

    const action = config.actionProps as Record<string, unknown>;
    const onClick = action.onClick;
    if (typeof onClick !== "function") throw new Error("Missing settings action");
    onClick();
    expect(harness.toastClose).toHaveBeenCalledWith("toast-1");
    expect(harness.refs[0]?.current).toBeNull();
    expect(harness.navigate).toHaveBeenCalledWith({ to: "/settings/providers" });
    onClick();
    expect(harness.navigate).toHaveBeenCalledTimes(2);

    const data = config.data as Record<string, unknown>;
    const onClose = data.onClose;
    if (typeof onClose !== "function") throw new Error("Missing close action");
    harness.refs[1]!.current = "updates-live";
    onClose();
    expect(harness.dismissKey).toHaveBeenCalledWith("updates-live");
    harness.refs[1]!.current = null;
    onClose();
    expect(harness.dismissKey).toHaveBeenCalledOnce();
  });

  it("closes active prompts on unmount and ignores empty cleanup", () => {
    harness.environments = [{ entry: { target: { local: true } } }];
    harness.refSeeds = [{ toastId: "toast-old", key: "old" }];
    renderNotification();
    const cleanup = harness.effects[0]?.();
    if (typeof cleanup === "function") cleanup();
    expect(harness.toastClose).toHaveBeenCalledWith("toast-old");
    expect(harness.refs[0]?.current).toBeNull();

    harness.refSeeds = [];
    renderNotification();
    const emptyCleanup = harness.effects[0]?.();
    if (typeof emptyCleanup === "function") emptyCleanup();
    expect(harness.toastClose).toHaveBeenCalledOnce();
  });

  it("gates dismissed, settling, seen, and already-active prompts", () => {
    harness.environments = [{ entry: { target: { local: true } } }];
    harness.notificationKey = "updates-dismissed";
    harness.dismissedKeys.add("updates-dismissed");
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastAdd).not.toHaveBeenCalled();

    harness.dismissedKeys.clear();
    harness.notificationKey = "updates-gated";
    harness.isAnySettling = true;
    harness.settleElapsed = false;
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastAdd).not.toHaveBeenCalled();

    harness.notificationKey = "updates-active";
    harness.isAnySettling = false;
    harness.refSeeds = [{ toastId: "toast-old", key: "updates-active" }];
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastAdd).not.toHaveBeenCalled();

    harness.refSeeds = [];
    harness.notificationKey = "updates-seen";
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastAdd).toHaveBeenCalledOnce();
    harness.toastAdd.mockClear();
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastAdd).not.toHaveBeenCalled();
  });

  it("closes stale untouched prompts but preserves gated or interacted prompts", () => {
    harness.environments = [{ entry: { target: { local: true } } }];
    harness.notificationKey = null;
    harness.refSeeds = [{ toastId: "toast-old", key: "old" }, null, false];
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastClose).toHaveBeenCalledWith("toast-old");

    harness.toastClose.mockClear();
    harness.notificationKey = "updates-resettling";
    harness.isAnySettling = true;
    harness.settleElapsed = false;
    harness.refSeeds = [{ toastId: "toast-old", key: "old" }, null, false];
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastClose).not.toHaveBeenCalled();

    harness.isAnySettling = false;
    harness.notificationKey = "updates-interacted";
    harness.refSeeds = [{ toastId: "toast-old", key: "old" }, null, true];
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastClose).not.toHaveBeenCalled();

    harness.notificationKey = "old";
    harness.refSeeds = [{ toastId: "toast-old", key: "old" }, null, false];
    renderNotification();
    harness.effects[3]?.();
    expect(harness.toastClose).not.toHaveBeenCalled();
  });
});
