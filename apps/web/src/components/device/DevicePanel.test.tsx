// @vitest-environment happy-dom

import type { DeviceCatalog, DeviceSnapshot } from "@bibcode/contracts";
import { EnvironmentId } from "@bibcode/contracts";
import * as Cause from "effect/Cause";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;
type CommandResult = { _tag: "Success"; value: unknown } | { _tag: "Failure"; error: unknown };

const h = vi.hoisted(() => ({
  availability: { known: true, supported: true, enabled: true },
  catalogQuery: null as unknown as {
    data: unknown;
    emission: unknown;
    error: string | null;
    isPending: boolean;
    refresh: () => void;
    revalidate: () => void;
  },
  calls: [] as Array<{ command: string; input: unknown }>,
  results: {} as Record<string, CommandResult>,
  navigations: [] as unknown[],
  screens: [] as unknown[],
  stillImages: null as string | null,
}));

vi.mock("./deviceAvailability", () => ({
  useDevicePreviewsAvailability: () => h.availability,
}));

vi.mock("../../state/device", () => ({
  deviceEnvironment: {
    catalog: (args: unknown) => ({ __q: "catalog", args }),
    open: { __cmd: "open" },
    close: { __cmd: "close" },
    rotate: { __cmd: "rotate" },
    input: { __cmd: "input" },
  },
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: () => h.catalogQuery,
}));

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: { __cmd: string }) => (input: unknown) => {
    h.calls.push({ command: command.__cmd, input });
    return Promise.resolve(h.results[command.__cmd] ?? { _tag: "Success", value: undefined });
  },
}));

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: () => false,
  squashAtomCommandFailure: (result: { error?: unknown }) => result.error,
  isQueryTransportCutoff: () => false,
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => (target: unknown) => {
    h.navigations.push(target);
    return Promise.resolve();
  },
}));

vi.mock("./DeviceScreen", async () => {
  const { useEffect } = await import("react");
  return {
    DeviceScreen: (props: AnyProps) => {
      h.screens.push(props.device);
      const onStillImages = props.onStillImages as (reason: string | null) => void;
      useEffect(() => onStillImages(h.stillImages), [onStillImages]);
      return <div data-device-screen />;
    },
  };
});

vi.mock("../ui/select", () => ({
  Select: (props: AnyProps) => (
    <div data-select data-disabled={Boolean(props.disabled)} data-value={props.value as string}>
      {props.children as ReactNode}
    </div>
  ),
  SelectTrigger: (props: AnyProps) => (
    <button type="button" aria-label={props["aria-label"] as string}>
      {props.children as ReactNode}
    </button>
  ),
  SelectValue: (props: AnyProps) => <span>{props.children as ReactNode}</span>,
  SelectPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  SelectItem: (props: AnyProps) => (
    <div data-item={props.value as string}>{props.children as ReactNode}</div>
  ),
}));

import { DevicePanel, rotatedOrientation, selectShownDevice } from "./DevicePanel";

const ENVIRONMENT_ID = EnvironmentId.make("environment-work");
const PIXEL = { name: "Pixel_8", displayName: "Pixel 8" };

function catalog(overrides: Partial<DeviceCatalog> = {}): DeviceCatalog {
  return {
    availability: { kind: "ready", message: "Ready." },
    avds: [PIXEL],
    devices: [],
    ...overrides,
  };
}

function snapshot(overrides: Partial<DeviceSnapshot> = {}): DeviceSnapshot {
  return {
    deviceId: "d-1",
    avdName: "Pixel_8",
    state: "ready",
    orientation: "portrait",
    width: 1080,
    height: 2400,
    ...overrides,
  };
}

function withCatalog(value: DeviceCatalog | null) {
  h.catalogQuery = {
    data: value,
    emission: value === null ? { _tag: "Initial" } : { _tag: "Success", value },
    error: null,
    isPending: false,
    refresh: vi.fn(),
    revalidate: vi.fn(),
  };
}

const mounted: Array<{ root: Root; container: HTMLDivElement }> = [];

async function render(): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mounted.push({ root, container });
  await act(async () => {
    root.render(<DevicePanel environmentId={ENVIRONMENT_ID} />);
  });
  return container;
}

function button(container: HTMLElement, text: string): HTMLButtonElement {
  const found = [...container.querySelectorAll("button")].find(
    (candidate) =>
      candidate.textContent?.trim() === text || candidate.getAttribute("aria-label") === text,
  );
  if (!found) throw new Error(`No button "${text}"`);
  return found;
}

async function click(element: HTMLElement) {
  await act(async () => {
    element.click();
  });
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.availability = { known: true, supported: true, enabled: true };
  h.calls.length = 0;
  h.results = { open: { _tag: "Success", value: { deviceId: "d-new" } } };
  h.navigations.length = 0;
  h.screens.length = 0;
  withCatalog(catalog());
});

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    await act(async () => entry.root.unmount());
    entry.container.remove();
  }
});

describe("DevicePanel", () => {
  it("shows the server's message and no Start button when the SDK is missing", async () => {
    withCatalog(
      catalog({
        availability: {
          kind: "sdkMissing",
          message: "No Android SDK found on this server. Set ANDROID_HOME and restart it.",
        },
        avds: [],
      }),
    );
    const container = await render();
    expect(container.textContent).toContain(
      "No Android SDK found on this server. Set ANDROID_HOME and restart it.",
    );
    expect(container.textContent).toContain("Android SDK not found");
    expect(() => button(container, "Start")).toThrow();
  });

  it("offers the AVDs and an enabled Start when no device runs", async () => {
    const container = await render();
    expect(container.querySelector("[aria-label='Virtual device']")?.textContent).toBe("Pixel 8");
    expect(container.querySelector("[data-item='Pixel_8']")).not.toBeNull();
    const start = button(container, "Start");
    expect(start.disabled).toBe(false);
    await click(start);
    expect(h.calls).toEqual([
      { command: "open", input: { environmentId: ENVIRONMENT_ID, input: { avdName: "Pixel_8" } } },
    ]);
  });

  it("shows a booting device with Stop enabled and rotate explaining why it waits", async () => {
    withCatalog(catalog({ devices: [snapshot({ state: "booting" })] }));
    const container = await render();
    expect(container.querySelector("[role='status']")?.textContent).toBe("Booting…");
    expect(button(container, "Stop").disabled).toBe(false);
    const rotate = button(container, "Rotate left");
    expect(rotate.disabled).toBe(true);
    expect(rotate.closest("[title]")?.getAttribute("title")).toBe(
      "Available once the device has booted",
    );
    expect(button(container, "Start").closest("[title]")?.getAttribute("title")).toBe(
      "Stop the running device to start another",
    );
    expect(h.screens).toEqual([]);
  });

  it("explains disabled controls before a device is started", async () => {
    const container = await render();
    expect(button(container, "Stop").closest("[title]")?.getAttribute("title")).toBe(
      "Start the device first",
    );
    expect(button(container, "Home").closest("[title]")?.getAttribute("title")).toBe(
      "Start the device first",
    );
  });

  it("shows a failed device's reason and lets Start try again", async () => {
    h.results.open = { _tag: "Success", value: { deviceId: "d-1" } };
    const container = await render();
    await click(button(container, "Start"));
    withCatalog(catalog({ devices: [snapshot({ state: "failed", failure: "exited: 1" })] }));
    await act(async () => {
      mounted[0]!.root.render(<DevicePanel environmentId={ENVIRONMENT_ID} />);
    });
    expect(container.querySelector("[role='status']")?.textContent).toBe("Failed: exited: 1");
    expect(button(container, "Start").disabled).toBe(false);
  });

  it("names the running AVD on a device limit and offers to stop it", async () => {
    withCatalog(
      catalog({
        avds: [PIXEL, { name: "Tablet", displayName: "Tablet" }],
        devices: [],
      }),
    );
    h.results.open = {
      _tag: "Failure",
      error: { _tag: "DeviceLimitError", running: "Pixel_8", message: "busy" },
    };
    const container = await render();
    await click(button(container, "Start"));
    // Another client started Pixel_8 meanwhile; the catalog shows it.
    withCatalog(
      catalog({
        avds: [PIXEL, { name: "Tablet", displayName: "Tablet" }],
        devices: [snapshot({ deviceId: "d-9", state: "ready" })],
      }),
    );
    await act(async () => {
      mounted[0]!.root.render(<DevicePanel environmentId={ENVIRONMENT_ID} />);
    });
    expect(container.querySelector("[role='alert']")?.textContent).toContain(
      "Pixel_8 is already running. Stop it before starting another device.",
    );
    await click(button(container, "Stop Pixel_8"));
    expect(h.calls.at(-1)).toEqual({
      command: "close",
      input: { environmentId: ENVIRONMENT_ID, input: { deviceId: "d-9" } },
    });
  });

  it("shows the screen for a ready device and sends Home as a key press", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    expect(h.screens).toHaveLength(1);
    await click(button(container, "Home"));
    // The queue flushes on the next animation frame.
    await act(async () => {
      await new Promise((resolve) => requestAnimationFrame(() => resolve(undefined)));
    });
    expect(h.calls).toContainEqual({
      command: "input",
      input: {
        environmentId: ENVIRONMENT_ID,
        input: {
          deviceId: "d-1",
          events: [
            { type: "key", key: "home", action: "down" },
            { type: "key", key: "home", action: "up" },
          ],
        },
      },
    });
  });

  it("shows why the screen is still images in the toolbar, off the screen", async () => {
    h.stillImages = "unsupported";
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    const badge = container.querySelector("[role=toolbar] [data-still-images]");
    expect(badge?.textContent).toBe("Still images");
    expect(badge?.getAttribute("data-still-images")).toBe("unsupported");
    expect(container.querySelector("[data-device-screen] [data-still-images]")).toBeNull();
    h.stillImages = null;
  });

  it("shows no still-images badge while the screen shows video", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    expect(container.querySelector("[data-still-images]")).toBeNull();
  });

  it("rotates a ready device to the next orientation", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    await click(button(container, "Rotate left"));
    expect(h.calls.at(-1)).toEqual({
      command: "rotate",
      input: {
        environmentId: ENVIRONMENT_ID,
        input: { deviceId: "d-1", orientation: "landscape" },
      },
    });
  });

  it("says previews are off, with a way to Settings, while the setting is off", async () => {
    h.availability = { known: true, supported: true, enabled: false };
    const container = await render();
    expect(container.textContent).toContain(
      "Device previews are off. Turn them on in Settings → Diagnostics → Device previews.",
    );
    await click(button(container, "Open settings"));
    expect(h.navigations).toEqual([
      {
        to: "/settings/diagnostics",
        search: { environment: ENVIRONMENT_ID },
        hash: "device-previews",
      },
    ]);
  });

  it("reads a catalog that ended with 'off' again once the setting is on", async () => {
    const error = { _tag: "DevicePreviewsDisabledError", message: "off" };
    h.catalogQuery = {
      data: null,
      emission: { _tag: "Failure", cause: Cause.fail(error) },
      error: "off",
      isPending: false,
      refresh: vi.fn(),
      revalidate: vi.fn(),
    };
    const container = await render();
    expect(h.catalogQuery.revalidate).toHaveBeenCalledTimes(1);
    expect(container.textContent).toContain(
      "Waiting for the server to apply the Device previews setting…",
    );
  });
});

describe("device selection helpers", () => {
  it("cycles orientation left and right", () => {
    expect(rotatedOrientation("portrait", "left")).toBe("landscape");
    expect(rotatedOrientation("reverseLandscape", "left")).toBe("portrait");
    expect(rotatedOrientation("portrait", "right")).toBe("reverseLandscape");
  });

  it("prefers the running device, else the followed failed one", () => {
    const failed = snapshot({ deviceId: "a", state: "failed" });
    const stopped = snapshot({ deviceId: "b", state: "stopped" });
    const booting = snapshot({ deviceId: "c", state: "booting" });
    expect(selectShownDevice(catalog({ devices: [failed, booting] }), "a")).toBe(booting);
    expect(selectShownDevice(catalog({ devices: [failed, stopped] }), "a")).toBe(failed);
    expect(selectShownDevice(catalog({ devices: [failed, stopped] }), "b")).toBeNull();
    expect(selectShownDevice(catalog({ devices: [failed] }), null)).toBeNull();
  });
});
