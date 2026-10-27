// @vitest-environment happy-dom

import type {
  DeviceAvailabilityKind,
  DeviceCatalog,
  DeviceImage,
  DevicePlatform,
  DevicePlatformCatalog,
  DeviceSnapshot,
} from "@coddy/contracts";
import { EnvironmentId } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type AnyProps = Record<string, unknown>;
type CommandResult = { _tag: "Success"; value: unknown } | { _tag: "Failure"; error: unknown };
/** A fixed result, or one made from the command's input, possibly settled later. */
type CommandReply = CommandResult | ((input: unknown) => CommandResult | Promise<CommandResult>);

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
  results: {} as Record<string, CommandReply>,
  navigations: [] as unknown[],
  screens: [] as unknown[],
  stillImages: null as { reason: string; message: string } | null,
  screenProps: null as AnyProps | null,
  catalogListeners: new Set<() => void>(),
  selectValueChange: null as unknown,
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

// Subscribed like the real query, so a catalog change re-renders the pane itself, not only a
// parent re-render (which the compiled DevicePanel does not pass on to an unchanged session).
vi.mock("../../state/query", async () => {
  const { useSyncExternalStore } = await import("react");
  const subscribe = (listener: () => void) => {
    h.catalogListeners.add(listener);
    return () => h.catalogListeners.delete(listener);
  };
  return {
    useEnvironmentQuery: () => useSyncExternalStore(subscribe, () => h.catalogQuery),
  };
});

vi.mock("../../state/use-atom-command", () => ({
  useAtomCommand: (command: { __cmd: string }) => (input: unknown) => {
    h.calls.push({ command: command.__cmd, input });
    const reply = h.results[command.__cmd];
    return Promise.resolve(
      typeof reply === "function" ? reply(input) : (reply ?? { _tag: "Success", value: undefined }),
    );
  },
}));

vi.mock("@coddy/client-runtime/state/runtime", () => ({
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
      h.screenProps = props;
      const onStillImages = props.onStillImages as (
        still: { reason: string; message: string } | null,
      ) => void;
      useEffect(() => onStillImages(h.stillImages), [onStillImages]);
      return <div data-device-screen />;
    },
  };
});

vi.mock("../ui/select", () => ({
  Select: (props: AnyProps) => {
    h.selectValueChange = props.onValueChange as (value: unknown) => void;
    return (
      <div data-select data-disabled={Boolean(props.disabled)} data-value={props.value as string}>
        {props.children as ReactNode}
      </div>
    );
  },
  SelectTrigger: (props: AnyProps) => (
    <button type="button" aria-label={props["aria-label"] as string}>
      {props.children as ReactNode}
    </button>
  ),
  SelectValue: (props: AnyProps) => <span>{props.children as ReactNode}</span>,
  SelectPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  SelectGroup: (props: AnyProps) => <div role="group">{props.children as ReactNode}</div>,
  SelectGroupLabel: (props: AnyProps) => <div data-group-label>{props.children as ReactNode}</div>,
  SelectItem: (props: AnyProps) => (
    <div
      role="option"
      data-item={props.value as string}
      aria-disabled={props.disabled ? "true" : undefined}
      onClick={() => {
        if (!props.disabled) (h.selectValueChange as (value: unknown) => void)(props.value);
      }}
    >
      {props.children as ReactNode}
    </div>
  ),
}));

import { DevicePanel, rotatedOrientation, selectedImageOf, selectShownDevice } from "./DevicePanel";

const ENVIRONMENT_ID = EnvironmentId.make("environment-work");
const PIXEL: DeviceImage = { id: "Pixel_8", displayName: "Pixel 8" };
const TABLET: DeviceImage = { id: "Tablet", displayName: "Tablet" };

/** An Android-only catalog, as a server without iOS support serializes it, unless `platforms`. */
function catalog(
  overrides: {
    readonly availability?: DevicePlatformCatalog["availability"];
    readonly images?: ReadonlyArray<DeviceImage>;
    readonly platforms?: ReadonlyArray<DevicePlatformCatalog>;
    readonly devices?: ReadonlyArray<DeviceSnapshot>;
  } = {},
): DeviceCatalog {
  return {
    platforms: overrides.platforms ?? [
      {
        platform: "android",
        availability: overrides.availability ?? { kind: "ready", message: "Ready." },
        images: overrides.images ?? [PIXEL],
      },
    ],
    devices: overrides.devices ?? [],
  };
}

function platform(
  name: DevicePlatform,
  kind: DeviceAvailabilityKind,
  message: string,
  images: ReadonlyArray<DeviceImage>,
): DevicePlatformCatalog {
  return { platform: name, availability: { kind, message }, images };
}

function image(id: string, displayName: string, inUseElsewhere = false): DeviceImage {
  return inUseElsewhere ? { id, displayName, inUseElsewhere: true } : { id, displayName };
}

const IPHONE = image("UDID-1", "iPhone 17 Pro · iOS 27.0");

/** A catalog with one ready iPhone simulator running on an iOS platform of `kind`. */
function readyIosDevice(
  overrides: Partial<DeviceSnapshot> = {},
  kind: DeviceAvailabilityKind = "ready",
  message = "Ready.",
): DeviceCatalog {
  return catalog({
    platforms: [platform("ios", kind, message, [IPHONE])],
    devices: [snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1", ...overrides })],
  });
}

function snapshot(overrides: Partial<DeviceSnapshot> = {}): DeviceSnapshot {
  return {
    deviceId: "d-1",
    platform: "android",
    imageId: "Pixel_8",
    state: "ready",
    orientation: "portrait",
    width: 1080,
    height: 2400,
    canRotate: true,
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
  for (const listener of h.catalogListeners) listener();
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

function group(container: HTMLElement, label: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>("[role='group']")].find(
    (candidate) => candidate.querySelector("[data-group-label]")?.textContent === label,
  );
  if (!found) throw new Error(`No group "${label}"`);
  return found;
}

function option(container: HTMLElement, text: string | RegExp): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>("[role='option']")].find((candidate) =>
    typeof text === "string"
      ? candidate.textContent === text
      : text.test(candidate.textContent ?? ""),
  );
  if (!found) throw new Error(`No option "${String(text)}"`);
  return found;
}

/** The tooltip a control carries: its own title, or its disabled wrapper's. */
const reasonOf = (element: HTMLElement) => element.closest("[title]")?.getAttribute("title");

const flushInput = () =>
  act(async () => {
    await new Promise((resolve) => requestAnimationFrame(() => resolve(undefined)));
  });

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.availability = { known: true, supported: true, enabled: true };
  h.calls.length = 0;
  h.results = {
    open: { _tag: "Success", value: { deviceId: "d-new" } },
    // A device that turns as asked.
    rotate: (input) => ({
      _tag: "Success",
      value: { orientation: (input as { input: { orientation: string } }).input.orientation },
    }),
  };
  h.navigations.length = 0;
  h.screens.length = 0;
  h.screenProps = null;
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
        images: [],
      }),
    );
    const container = await render();
    expect(container.textContent).toContain(
      "No Android SDK found on this server. Set ANDROID_HOME and restart it.",
    );
    expect(container.textContent).toContain("Android SDK not found");
    expect(() => button(container, "Start")).toThrow();
  });

  it("offers the images and an enabled Start when no device runs", async () => {
    const container = await render();
    expect(container.querySelector("[aria-label='Virtual device']")?.textContent).toBe("Pixel 8");
    expect(container.querySelector("[data-item='android:Pixel_8']")).not.toBeNull();
    expect(container.querySelector("[data-select]")?.getAttribute("data-value")).toBe(
      "android:Pixel_8",
    );
    const start = button(container, "Start");
    expect(start.disabled).toBe(false);
    await click(start);
    expect(h.calls).toEqual([
      {
        command: "open",
        input: {
          environmentId: ENVIRONMENT_ID,
          input: { platform: "android", imageId: "Pixel_8" },
        },
      },
    ]);
  });

  it("shows the server's message when the image already runs outside Coddy Studio", async () => {
    const message =
      "This simulator is already running outside Coddy Studio. Shut it down in Simulator, then open it here.";
    h.results.open = {
      _tag: "Failure",
      error: { _tag: "DeviceBusyError", imageId: "Pixel_8", message },
    };
    const container = await render();
    await click(button(container, "Start"));
    expect(container.querySelector("[role='alert']")?.textContent).toContain(message);
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
    await act(async () => {
      withCatalog(catalog({ devices: [snapshot({ state: "failed", failure: "exited: 1" })] }));
    });
    expect(container.querySelector("[role='status']")?.textContent).toBe("Failed: exited: 1");
    expect(button(container, "Start").disabled).toBe(false);
  });

  it("names the running device on a device limit and offers to stop it", async () => {
    withCatalog(catalog({ images: [PIXEL, TABLET] }));
    // `running` is the display name, not the image id.
    h.results.open = {
      _tag: "Failure",
      error: { _tag: "DeviceLimitError", running: "Pixel 8", message: "busy" },
    };
    const container = await render();
    await click(button(container, "Start"));
    // Another client started Pixel_8 meanwhile; the catalog shows it.
    await act(async () => {
      withCatalog(
        catalog({
          images: [PIXEL, TABLET],
          devices: [snapshot({ deviceId: "d-9", state: "ready" })],
        }),
      );
    });
    expect(container.querySelector("[role='alert']")?.textContent).toContain(
      "Pixel 8 is already running. Stop it before starting another device.",
    );
    await click(button(container, "Stop Pixel 8"));
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
    h.stillImages = { reason: "unsupported", message: "No video here." };
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

  it("says when a device kept its orientation, until the next rotate", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    // The launcher refuses landscape: the device ends where it was.
    h.results.rotate = { _tag: "Success", value: { orientation: "portrait" } };
    const container = await render();
    expect(container.querySelector("[data-rotate-note]")).toBeNull();
    await click(button(container, "Rotate left"));
    expect(container.querySelector("[role='status'][data-rotate-note]")?.textContent).toBe(
      "Pixel 8 didn't rotate. The app on screen may lock its orientation, or auto-rotate may be off on the device.",
    );
    // The next press clears it, and a turn that lands says nothing.
    h.results.rotate = { _tag: "Success", value: { orientation: "landscape" } };
    await click(button(container, "Rotate left"));
    expect(container.querySelector("[data-rotate-note]")).toBeNull();
    expect(container.querySelector("[role='alert']")).toBeNull();
  });

  /** A rotate command whose calls the test settles one by one, in the order it chooses. */
  function pendingRotates() {
    const settle: Array<(result: CommandResult) => void> = [];
    h.results.rotate = () =>
      new Promise<CommandResult>((resolve) => {
        settle.push(resolve);
      });
    return {
      turn: async (press: number, orientation: string) => {
        await act(async () => settle[press]!({ _tag: "Success", value: { orientation } }));
      },
      fail: async (press: number) => {
        await act(async () =>
          settle[press]!({
            _tag: "Failure",
            error: { _tag: "DeviceEmulatorError", message: "The emulator did not respond." },
          }),
        );
      },
    };
  }
  const rotateCalls = () => h.calls.filter((call) => call.command === "rotate");
  const rotateTargets = () =>
    rotateCalls().map(
      (call) => (call.input as { input: { orientation: string } }).input.orientation,
    );

  it("sends one rotate for two quick presses", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const rotates = pendingRotates();
    const container = await render();
    await click(button(container, "Rotate left"));
    await click(button(container, "Rotate left"));
    await click(button(container, "Rotate right"));
    expect(rotateTargets()).toEqual(["landscape"]);
    await rotates.turn(0, "landscape");
    expect(rotateTargets()).toEqual(["landscape"]);
  });

  it("sends one rotate for presses that land before the buttons render held", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    pendingRotates();
    const container = await render();
    const left = button(container, "Rotate left");
    const right = button(container, "Rotate right");
    // One act renders once, after both presses.
    await act(async () => {
      left.click();
      left.click();
      right.click();
    });
    expect(rotateTargets()).toEqual(["landscape"]);
  });

  it("holds both Rotate buttons with the reason while a turn is in flight", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const rotates = pendingRotates();
    const container = await render();
    for (const label of ["Rotate left", "Rotate right"]) {
      expect(button(container, label).disabled).toBe(false);
    }
    await click(button(container, "Rotate left"));
    for (const label of ["Rotate left", "Rotate right"]) {
      const rotate = button(container, label);
      expect(rotate.disabled).toBe(true);
      expect(reasonOf(rotate)).toBe("Rotating…");
    }
    // Only Rotate waits; the other controls stay usable.
    expect(button(container, "Home").disabled).toBe(false);
    await rotates.turn(0, "landscape");
    for (const label of ["Rotate left", "Rotate right"]) {
      const rotate = button(container, label);
      expect(rotate.disabled).toBe(false);
      expect(reasonOf(rotate)).toBe(label);
    }
  });

  it("lets Rotate be pressed again after a turn fails, and says why it failed", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const rotates = pendingRotates();
    const container = await render();
    await click(button(container, "Rotate left"));
    expect(button(container, "Rotate left").disabled).toBe(true);
    await rotates.fail(0);
    expect(container.querySelector("[role='alert']")?.textContent).toContain(
      "The emulator did not respond.",
    );
    expect(button(container, "Rotate left").disabled).toBe(false);
    expect(button(container, "Rotate right").disabled).toBe(false);
    await click(button(container, "Rotate left"));
    expect(rotateTargets()).toEqual(["landscape", "landscape"]);
  });

  it("holds Rotate only for the device that is turning", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("android", "ready", "Ready.", [PIXEL]),
          platform("ios", "ready", "Ready.", [IPHONE]),
        ],
        devices: [
          snapshot({ deviceId: "a-1" }),
          snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1" }),
        ],
      }),
    );
    const rotates = pendingRotates();
    const container = await render();
    await click(button(container, "Rotate left"));
    expect(rotateCalls()).toHaveLength(1);
    // The other device is not turning, so its buttons are free.
    await click(option(container, "iPhone 17 Pro · iOS 27.0"));
    expect(button(container, "Rotate left").disabled).toBe(false);
    // Back on the one that is still turning, the hold is still there.
    await click(option(container, "Pixel 8"));
    expect(button(container, "Rotate left").disabled).toBe(true);
    expect(reasonOf(button(container, "Rotate left"))).toBe("Rotating…");
    await click(button(container, "Rotate right"));
    expect(rotateCalls()).toHaveLength(1);
    await rotates.turn(0, "landscape");
    expect(button(container, "Rotate left").disabled).toBe(false);
  });

  it("turns from the orientation the last turn produced while the catalog still shows the old one", async () => {
    // The catalog is not refreshed in this test: it keeps saying portrait.
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    await click(button(container, "Rotate left"));
    await click(button(container, "Rotate left"));
    await click(button(container, "Rotate right"));
    await click(button(container, "Rotate right"));
    // portrait -> landscape -> reverse portrait -> landscape -> portrait
    expect(rotateTargets()).toEqual(["landscape", "reversePortrait", "landscape", "portrait"]);
  });

  it("starts from the orientation the device kept when a turn is refused", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    h.results.rotate = { _tag: "Success", value: { orientation: "portrait" } };
    await click(button(container, "Rotate left"));
    h.results.rotate = (input) => ({
      _tag: "Success",
      value: { orientation: (input as { input: { orientation: string } }).input.orientation },
    });
    await click(button(container, "Rotate left"));
    expect(rotateTargets()).toEqual(["landscape", "landscape"]);
  });

  it("turns from what the catalog reports once it has caught up or moved on", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    await click(button(container, "Rotate left"));
    // The catalog follows the turn.
    await act(async () => {
      withCatalog(catalog({ devices: [snapshot({ orientation: "landscape" })] }));
    });
    await click(button(container, "Rotate left"));
    // The catalog follows that turn, then the app turns the screen back by itself: the result
    // held for the turn must not come back.
    await act(async () => {
      withCatalog(catalog({ devices: [snapshot({ orientation: "reversePortrait" })] }));
    });
    await act(async () => {
      withCatalog(catalog({ devices: [snapshot({ orientation: "landscape" })] }));
    });
    await click(button(container, "Rotate left"));
    expect(rotateTargets()).toEqual(["landscape", "reversePortrait", "reversePortrait"]);
  });

  it("does not bring back a turn the catalog reported before the result arrived", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const rotates = pendingRotates();
    const container = await render();
    await click(button(container, "Rotate left"));
    // The catalog reports the turn first, then the app turns the screen back by itself.
    await act(async () => {
      withCatalog(catalog({ devices: [snapshot({ orientation: "landscape" })] }));
    });
    await rotates.turn(0, "landscape");
    await act(async () => {
      withCatalog(catalog({ devices: [snapshot({ orientation: "portrait" })] }));
    });
    h.results.rotate = (input) => ({
      _tag: "Success",
      value: { orientation: (input as { input: { orientation: string } }).input.orientation },
    });
    await click(button(container, "Rotate left"));
    expect(rotateTargets()).toEqual(["landscape", "landscape"]);
  });

  it("does not use one device's last turn for another", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("android", "ready", "Ready.", [PIXEL]),
          platform("ios", "ready", "Ready.", [IPHONE]),
        ],
        devices: [
          snapshot({ deviceId: "a-1" }),
          snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1" }),
        ],
      }),
    );
    const container = await render();
    await click(button(container, "Rotate left"));
    await click(option(container, "iPhone 17 Pro · iOS 27.0"));
    await click(button(container, "Rotate left"));
    const devices = rotateCalls().map(
      (call) => (call.input as { input: { deviceId: string } }).input.deviceId,
    );
    expect(devices).toEqual(["a-1", "i-1"]);
    expect(rotateTargets()).toEqual(["landscape", "landscape"]);
  });

  it("lets the kept-orientation note go after a few seconds", async () => {
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      withCatalog(catalog({ devices: [snapshot()] }));
      h.results.rotate = { _tag: "Success", value: { orientation: "portrait" } };
      const container = await render();
      await click(button(container, "Rotate right"));
      expect(container.querySelector("[data-rotate-note]")).not.toBeNull();
      await act(async () => {
        vi.advanceTimersByTime(4_000);
      });
      expect(container.querySelector("[data-rotate-note]")).not.toBeNull();
      await act(async () => {
        vi.advanceTimersByTime(1_000);
      });
      expect(container.querySelector("[data-rotate-note]")).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("says previews are off, with a way to Settings, while the setting is off", async () => {
    h.availability = { known: true, supported: true, enabled: false };
    const container = await render();
    expect(container.textContent).toContain(
      "Device previews are off. Turn them on in Settings → Mobile emulators.",
    );
    await click(button(container, "Open settings"));
    expect(h.navigations).toEqual([
      {
        to: "/settings/mobile",
        hash: `device-previews-${ENVIRONMENT_ID}`,
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

describe("DevicePanel with iOS simulators", () => {
  it("groups images by platform and keeps an unavailable platform's message", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("android", "sdkMissing", "Install the Android SDK on the server.", []),
          platform("ios", "ready", "Ready.", [IPHONE]),
        ],
      }),
    );
    const container = await render();
    const groups = [...container.querySelectorAll("[role='group'] [data-group-label]")];
    expect(groups.map((label) => label.textContent)).toEqual(["Android", "iOS"]);
    const android = group(container, "Android");
    expect(android.textContent).toContain("Install the Android SDK on the server.");
    // The reason is text under the heading, not an unavailable option.
    expect(android.querySelector("[role='option']")).toBeNull();
    const iphone = option(container, "iPhone 17 Pro · iOS 27.0");
    expect(iphone.getAttribute("aria-disabled")).toBeNull();
    expect(group(container, "iOS").contains(iphone)).toBe(true);
    // The ready platform's image is picked, and Start opens it.
    expect(container.querySelector("[aria-label='Virtual device']")?.textContent).toBe(
      "iPhone 17 Pro · iOS 27.0",
    );
    expect(button(container, "Start").disabled).toBe(false);
  });

  it("shows only the platforms the catalog lists", async () => {
    const container = await render();
    const groups = [...container.querySelectorAll("[role='group'] [data-group-label]")];
    expect(groups.map((label) => label.textContent)).toEqual(["Android"]);
  });

  it("disables a simulator running outside Coddy Studio", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("ios", "ready", "Ready.", [
            image("UDID-1", "iPhone 17 Pro · iOS 27.0", true),
            image("UDID-2", "iPhone Air · iOS 27.0"),
          ]),
        ],
      }),
    );
    const container = await render();
    const busy = option(container, /Running outside Coddy Studio/);
    expect(busy.textContent).toBe("iPhone 17 Pro · iOS 27.0 — Running outside Coddy Studio");
    expect(busy.getAttribute("aria-disabled")).toBe("true");
    // The busy simulator is not picked by default.
    expect(container.querySelector("[aria-label='Virtual device']")?.textContent).toBe(
      "iPhone Air · iOS 27.0",
    );
  });

  it("lists a view-only platform's simulators with why they are view only", async () => {
    const message =
      "Live input needs someone logged in on this Mac. Simulators open as screenshots.";
    withCatalog(catalog({ platforms: [platform("ios", "viewOnlyHeadless", message, [IPHONE])] }));
    const container = await render();
    expect(option(container, "iPhone 17 Pro · iOS 27.0").getAttribute("aria-disabled")).toBeNull();
    expect(group(container, "iOS").textContent).toContain(message);
    expect(button(container, "Start").disabled).toBe(false);
  });

  it("explains every platform when none can start a device", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("android", "sdkMissing", "Install the Android SDK on the server.", []),
          platform("ios", "xcodeMissing", "Install Xcode, then open it once to finish setup.", []),
        ],
      }),
    );
    const container = await render();
    expect(container.textContent).toContain("Android SDK not found");
    expect(container.textContent).toContain("Install the Android SDK on the server.");
    expect(container.textContent).toContain("Xcode not found");
    expect(container.textContent).toContain("Install Xcode, then open it once to finish setup.");
    expect(() => button(container, "Start")).toThrow();
  });

  it("shows iOS buttons and explains a disabled Rotate", async () => {
    withCatalog(readyIosDevice({ canRotate: false }));
    const container = await render();
    expect(() => button(container, "Back")).toThrow();
    expect(() => button(container, "Recents")).toThrow();
    expect(button(container, "Home").disabled).toBe(false);
    expect(button(container, "Lock").disabled).toBe(false);
    expect(button(container, "Volume up").disabled).toBe(false);
    expect(button(container, "Volume down").disabled).toBe(false);
    expect(button(container, "Paste").disabled).toBe(false);
    for (const label of ["Rotate left", "Rotate right"]) {
      const rotate = button(container, label);
      expect(rotate.disabled).toBe(true);
      expect(reasonOf(rotate)).toBe("Rotation isn't available for iOS simulators yet");
    }
  });

  it("sends Lock as the power key and rotates a simulator that can turn", async () => {
    withCatalog(readyIosDevice());
    const container = await render();
    expect(button(container, "Rotate left").disabled).toBe(false);
    await click(button(container, "Lock"));
    await flushInput();
    expect(h.calls).toContainEqual({
      command: "input",
      input: {
        environmentId: ENVIRONMENT_ID,
        input: {
          deviceId: "i-1",
          events: [
            { type: "key", key: "power", action: "down" },
            { type: "key", key: "power", action: "up" },
          ],
        },
      },
    });
  });

  it("keeps Android's buttons", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    for (const label of ["Back", "Home", "Recents", "Power", "Paste"]) {
      expect(button(container, label).disabled).toBe(false);
    }
    expect(() => button(container, "Lock")).toThrow();
  });

  it("view-only iOS shows why on the screen and disables input controls", async () => {
    const message =
      "Xcode 27.1 changed the simulator interfaces Coddy Studio uses. Simulators open as screenshots without touch input.";
    withCatalog(readyIosDevice({ canRotate: false }, "xcodeUnsupported", message));
    const container = await render();
    expect(h.screenProps?.inputDisabledReason).toBe(message);
    for (const label of ["Home", "Lock", "Volume up", "Volume down", "Paste", "Rotate left"]) {
      const control = button(container, label);
      expect(control.disabled).toBe(true);
      expect(reasonOf(control)).toBe("This simulator is view only");
    }
    await click(button(container, "Home"));
    await flushInput();
    expect(h.calls.filter((call) => call.command === "input")).toEqual([]);
  });

  it("gives a live simulator's screen no view-only reason", async () => {
    withCatalog(readyIosDevice());
    await render();
    expect(h.screenProps?.inputDisabledReason).toBeNull();
    expect(h.screenProps?.name).toBe("iPhone 17 Pro · iOS 27.0");
  });

  it("opens the selected image on its platform", async () => {
    withCatalog(catalog({ platforms: [platform("ios", "ready", "Ready.", [IPHONE])] }));
    const container = await render();
    await click(button(container, "Start"));
    expect(h.calls).toEqual([
      {
        command: "open",
        input: { environmentId: ENVIRONMENT_ID, input: { platform: "ios", imageId: "UDID-1" } },
      },
    ]);
  });

  it("keeps the same image id apart on two platforms", async () => {
    const twin = image("Twin", "Twin on iOS");
    withCatalog(
      catalog({
        platforms: [
          platform("android", "ready", "Ready.", [image("Twin", "Twin on Android")]),
          platform("ios", "ready", "Ready.", [twin]),
        ],
        devices: [snapshot({ deviceId: "a-1", platform: "android", imageId: "Twin" })],
      }),
    );
    const container = await render();
    // The running Android twin is shown; picking the iOS twin shows nothing running.
    expect(container.querySelector("[data-select]")?.getAttribute("data-value")).toBe(
      "android:Twin",
    );
    expect(h.screens.at(-1)).toMatchObject({ deviceId: "a-1" });
    await click(option(container, "Twin on iOS"));
    expect(container.querySelector("[data-select]")?.getAttribute("data-value")).toBe("ios:Twin");
    expect(container.querySelector("[aria-label='Virtual device']")?.textContent).toBe(
      "Twin on iOS",
    );
    expect(container.querySelector("[data-device-screen]")).toBeNull();
    await click(button(container, "Start"));
    expect(h.calls.at(-1)).toEqual({
      command: "open",
      input: { environmentId: ENVIRONMENT_ID, input: { platform: "ios", imageId: "Twin" } },
    });
  });

  it("switches between an Android and an iOS device running at once", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("android", "ready", "Ready.", [PIXEL]),
          platform("ios", "ready", "Ready.", [IPHONE]),
        ],
        devices: [
          snapshot({ deviceId: "a-1" }),
          snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1" }),
        ],
      }),
    );
    const container = await render();
    expect(h.screens.at(-1)).toMatchObject({ deviceId: "a-1" });
    await click(option(container, "iPhone 17 Pro · iOS 27.0"));
    expect(h.screens.at(-1)).toMatchObject({ deviceId: "i-1" });
    await click(button(container, "Stop"));
    expect(h.calls.at(-1)).toEqual({
      command: "close",
      input: { environmentId: ENVIRONMENT_ID, input: { deviceId: "i-1" } },
    });
  });

  it("offers to show the device already running on the picked image's platform", async () => {
    withCatalog(catalog({ images: [PIXEL, TABLET], devices: [snapshot({ deviceId: "a-1" })] }));
    const container = await render();
    await click(option(container, "Tablet"));
    expect(container.textContent).toContain("Pixel 8 is running.");
    expect(reasonOf(button(container, "Start"))).toBe("Stop the running device to start another");
    await click(button(container, "Show Pixel 8"));
    expect(h.screens.at(-1)).toMatchObject({ deviceId: "a-1" });
  });

  it("stops the running device of the limit's platform, whatever its name", async () => {
    withCatalog(
      catalog({
        platforms: [
          platform("android", "ready", "Ready.", [PIXEL]),
          platform("ios", "ready", "Ready.", [IPHONE, image("UDID-2", "iPhone Air")]),
        ],
      }),
    );
    h.results.open = {
      _tag: "Failure",
      error: { _tag: "DeviceLimitError", running: "iPhone 17 Pro (renamed)", message: "busy" },
    };
    const container = await render();
    await click(option(container, "iPhone Air"));
    await click(button(container, "Start"));
    await act(async () => {
      withCatalog(
        catalog({
          platforms: [
            platform("android", "ready", "Ready.", [PIXEL]),
            platform("ios", "ready", "Ready.", [IPHONE, image("UDID-2", "iPhone Air")]),
          ],
          devices: [
            snapshot({ deviceId: "a-1" }),
            snapshot({ deviceId: "i-9", platform: "ios", imageId: "UDID-1" }),
          ],
        }),
      );
    });
    await click(button(container, "Stop iPhone 17 Pro (renamed)"));
    expect(h.calls.at(-1)).toEqual({
      command: "close",
      input: { environmentId: ENVIRONMENT_ID, input: { deviceId: "i-9" } },
    });
  });

  it("names a starting or stopping simulator by its display name, not its UDID", async () => {
    withCatalog(readyIosDevice({ state: "booting" }));
    const container = await render();
    expect(container.textContent).toContain("Starting iPhone 17 Pro · iOS 27.0.");
    expect(container.textContent).not.toContain("UDID-1");
    await act(async () => withCatalog(readyIosDevice({ state: "stopping" })));
    expect(container.textContent).toContain("Stopping iPhone 17 Pro · iOS 27.0…");
  });

  it("notes the Allow Paste prompt once, on the first iOS text that is pasted", async () => {
    withCatalog(readyIosDevice());
    const container = await render();
    expect(reasonOf(button(container, "Paste"))).toContain("Allow Paste");
    const onInput = h.screenProps?.onInput as (event: unknown) => void;
    // US-typeable text is typed as keys, so iOS never asks.
    await act(async () => onInput({ type: "text", text: "Hello, world!\n" }));
    expect(container.querySelector("[data-paste-note]")).toBeNull();
    await act(async () => onInput({ type: "text", text: "héllo" }));
    expect(container.querySelector("[data-paste-note]")?.textContent).toContain("Allow Paste");
    await click(button(container, "Dismiss"));
    expect(container.querySelector("[data-paste-note]")).toBeNull();
    await act(async () => onInput({ type: "text", text: "ü" }));
    expect(container.querySelector("[data-paste-note]")).toBeNull();
  });

  it("does not note Allow Paste on Android", async () => {
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    const onInput = h.screenProps?.onInput as (event: unknown) => void;
    await act(async () => onInput({ type: "text", text: "héllo" }));
    expect(container.querySelector("[data-paste-note]")).toBeNull();
  });

  it("drops clipboard text when the shown device changed while reading it", async () => {
    let resolveText!: (text: string) => void;
    const readText = vi.fn(
      () =>
        new Promise<string>((resolve) => {
          resolveText = resolve;
        }),
    );
    vi.stubGlobal("navigator", { ...navigator, clipboard: { readText } });
    try {
      const both = (iosState: DeviceSnapshot["state"]) =>
        catalog({
          platforms: [
            platform("android", "ready", "Ready.", [PIXEL]),
            platform("ios", "ready", "Ready.", [IPHONE]),
          ],
          devices: [
            snapshot({ deviceId: "a-1" }),
            snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1", state: iosState }),
          ],
        });
      withCatalog(both("ready"));
      const container = await render();
      await click(option(container, "iPhone 17 Pro · iOS 27.0"));
      await click(button(container, "Paste"));
      // The simulator stops and the user switches to the Android device meanwhile.
      await act(async () => withCatalog(both("stopped")));
      await click(option(container, "Pixel 8"));
      await act(async () => resolveText("héllo"));
      await flushInput();
      expect(h.calls.filter((call) => call.command === "input")).toEqual([]);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keeps the started image selected until the catalog lists its device", async () => {
    const platforms = [
      platform("android", "ready", "Ready.", [PIXEL]),
      platform("ios", "ready", "Ready.", [IPHONE]),
    ];
    withCatalog(catalog({ platforms, devices: [snapshot({ deviceId: "a-1" })] }));
    h.results.open = { _tag: "Success", value: { deviceId: "i-new" } };
    const container = await render();
    await click(option(container, "iPhone 17 Pro · iOS 27.0"));
    await click(button(container, "Start"));
    // The next catalog does not list the new simulator yet.
    await act(async () =>
      withCatalog(catalog({ platforms, devices: [snapshot({ deviceId: "a-1" })] })),
    );
    expect(container.querySelector("[data-select]")?.getAttribute("data-value")).toBe("ios:UDID-1");
    expect(container.querySelector("[data-device-screen]")).toBeNull();
    expect(button(container, "Home").disabled).toBe(true);
    await click(button(container, "Home"));
    await flushInput();
    expect(h.calls.filter((call) => call.command !== "open")).toEqual([]);
    // Once it is listed, the pane shows it.
    await act(async () =>
      withCatalog(
        catalog({
          platforms,
          devices: [
            snapshot({ deviceId: "a-1" }),
            snapshot({ deviceId: "i-new", platform: "ios", imageId: "UDID-1" }),
          ],
        }),
      ),
    );
    expect(h.screens.at(-1)).toMatchObject({ deviceId: "i-new" });
  });

  it("says so when the server lists no platforms", async () => {
    withCatalog(catalog({ platforms: [] }));
    const container = await render();
    expect(container.textContent).toContain("This server can't run device previews.");
  });

  it("pastes the clipboard from the Paste button", async () => {
    const readText = vi.fn(() => Promise.resolve("héllo"));
    vi.stubGlobal("navigator", { ...navigator, clipboard: { readText } });
    try {
      withCatalog(readyIosDevice());
      const container = await render();
      await click(button(container, "Paste"));
      await flushInput();
      expect(h.calls).toContainEqual({
        command: "input",
        input: {
          environmentId: ENVIRONMENT_ID,
          input: { deviceId: "i-1", events: [{ type: "text", text: "héllo" }] },
        },
      });
      expect(container.querySelector("[data-paste-note]")).not.toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("says how to paste when the clipboard cannot be read", async () => {
    const readText = vi.fn(() => Promise.reject(new Error("denied")));
    vi.stubGlobal("navigator", { ...navigator, clipboard: { readText } });
    try {
      withCatalog(catalog({ devices: [snapshot()] }));
      const container = await render();
      await click(button(container, "Paste"));
      expect(container.querySelector("[role='status'][data-input-error]")?.textContent).toBe(
        "Coddy Studio can't read the clipboard here. Click the screen and paste with Ctrl+V or ⌘V.",
      );
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("sends a release again when device.input refuses its batch", async () => {
    h.results.input = {
      _tag: "Failure",
      error: { _tag: "DeviceInputError", message: "The emulator accepted no input." },
    };
    withCatalog(catalog({ devices: [snapshot()] }));
    const container = await render();
    await click(button(container, "Home"));
    await flushInput();
    // The lane's send rejected, so the queue sent Home's release once more.
    expect(h.calls.filter((call) => call.command === "input").map((call) => call.input)).toEqual([
      {
        environmentId: ENVIRONMENT_ID,
        input: {
          deviceId: "d-1",
          events: [
            { type: "key", key: "home", action: "down" },
            { type: "key", key: "home", action: "up" },
          ],
        },
      },
      {
        environmentId: ENVIRONMENT_ID,
        input: { deviceId: "d-1", events: [{ type: "key", key: "home", action: "up" }] },
      },
    ]);
    expect(container.querySelector("[role='status'][data-input-error]")?.textContent).toBe(
      "The emulator accepted no input.",
    );
  });

  it("shows the server's reason when the simulator refuses input", async () => {
    h.results.input = {
      _tag: "Failure",
      error: {
        _tag: "DeviceInputError",
        message: "The simulator is not accepting keyboard input yet. Try again in a few seconds.",
      },
    };
    withCatalog(readyIosDevice());
    const container = await render();
    await click(button(container, "Home"));
    await flushInput();
    expect(container.querySelector("[role='status'][data-input-error]")?.textContent).toBe(
      "The simulator is not accepting keyboard input yet. Try again in a few seconds.",
    );
  });

  it("keeps an input refusal up for a few seconds, past the input accepted after it", async () => {
    const refusal =
      "This device can type only plain ASCII text. Accented letters, emoji, and other characters can't be sent from here yet.";
    const refusedAgain = "The emulator accepted no input.";
    h.results.input = (input) => {
      const sent = JSON.stringify(input);
      if (sent.includes("é")) {
        return { _tag: "Failure", error: { _tag: "DeviceInputError", message: refusal } };
      }
      if (sent.includes("home")) {
        return { _tag: "Failure", error: { _tag: "DeviceInputError", message: refusedAgain } };
      }
      return { _tag: "Success", value: undefined };
    };
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      withCatalog(catalog({ devices: [snapshot()] }));
      const container = await render();
      const onInput = h.screenProps?.onInput as (event: unknown) => void;
      const shown = () =>
        container.querySelector("[role='status'][data-input-error]")?.textContent ?? null;
      await act(async () => onInput({ type: "text", text: "é" }));
      await flushInput();
      expect(shown()).toBe(refusal);
      // The next keystroke is accepted: the refusal stays, so it can be read.
      await act(async () => onInput({ type: "text", text: "a" }));
      await flushInput();
      expect(shown()).toBe(refusal);
      await act(async () => {
        vi.advanceTimersByTime(4_000);
      });
      expect(shown()).toBe(refusal);
      // A newer refusal replaces it, and has its own few seconds.
      await click(button(container, "Home"));
      await flushInput();
      expect(shown()).toBe(refusedAgain);
      await act(async () => {
        vi.advanceTimersByTime(4_000);
      });
      expect(shown()).toBe(refusedAgain);
      await act(async () => {
        vi.advanceTimersByTime(1_000);
      });
      expect(shown()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("does not show one device's input refusal over another device", async () => {
    const refusal =
      "This device can type only plain ASCII text. Accented letters, emoji, and other characters can't be sent from here yet.";
    h.results.input = { _tag: "Failure", error: { _tag: "DeviceInputError", message: refusal } };
    // The note's 5 s timer cannot run out, so only the device it belongs to can hide it.
    vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
    try {
      withCatalog(
        catalog({
          platforms: [
            platform("android", "ready", "Ready.", [PIXEL]),
            platform("ios", "ready", "Ready.", [IPHONE]),
          ],
          devices: [
            snapshot({ deviceId: "a-1" }),
            snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1" }),
          ],
        }),
      );
      const container = await render();
      const shown = () =>
        container.querySelector("[role='status'][data-input-error]")?.textContent ?? null;
      expect(h.screens.at(-1)).toMatchObject({ deviceId: "a-1" });
      const onInput = h.screenProps?.onInput as (event: unknown) => void;
      await act(async () => onInput({ type: "text", text: "é" }));
      await flushInput();
      expect(shown()).toBe(refusal);
      await click(option(container, "iPhone 17 Pro · iOS 27.0"));
      expect(h.screens.at(-1)).toMatchObject({ deviceId: "i-1" });
      expect(shown()).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows the server's reason when an Android device refuses text it cannot type", async () => {
    const message =
      "This device can type only plain ASCII text. Accented letters, emoji, and other characters can't be sent from here yet.";
    h.results.input = { _tag: "Failure", error: { _tag: "DeviceInputError", message } };
    const readText = vi.fn(() => Promise.resolve("héllo"));
    vi.stubGlobal("navigator", { ...navigator, clipboard: { readText } });
    try {
      withCatalog(catalog({ devices: [snapshot()] }));
      const container = await render();
      await click(button(container, "Paste"));
      await flushInput();
      // The refusal is the server's own words, not the generic fallback.
      expect(container.querySelector("[role='status'][data-input-error]")?.textContent).toBe(
        message,
      );
      // Text holds nothing down, so the refused batch is not sent again as a release.
      expect(h.calls.filter((call) => call.command === "input").map((call) => call.input)).toEqual([
        {
          environmentId: ENVIRONMENT_ID,
          input: { deviceId: "d-1", events: [{ type: "text", text: "héllo" }] },
        },
      ]);
      // iOS's Allow Paste reminder does not apply to an Android device.
      expect(container.querySelector("[data-paste-note]")).toBeNull();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("device selection helpers", () => {
  it("cycles orientation left and right", () => {
    expect(rotatedOrientation("portrait", "left")).toBe("landscape");
    expect(rotatedOrientation("reverseLandscape", "left")).toBe("portrait");
    expect(rotatedOrientation("portrait", "right")).toBe("reverseLandscape");
  });

  it("shows the selected image's running device, else its followed failed one", () => {
    const pixel = { platform: "android", imageId: "Pixel_8" } as const;
    const failed = snapshot({ deviceId: "a", state: "failed" });
    const stopped = snapshot({ deviceId: "b", state: "stopped" });
    const booting = snapshot({ deviceId: "c", state: "booting" });
    expect(selectShownDevice(catalog({ devices: [failed, booting] }), pixel, "a")).toBe(booting);
    expect(selectShownDevice(catalog({ devices: [failed, stopped] }), pixel, "a")).toBe(failed);
    expect(selectShownDevice(catalog({ devices: [failed, stopped] }), pixel, "b")).toBeNull();
    expect(selectShownDevice(catalog({ devices: [failed] }), pixel, null)).toBeNull();
    const other = { platform: "ios", imageId: "Pixel_8" } as const;
    expect(selectShownDevice(catalog({ devices: [booting] }), other, null)).toBeNull();
  });

  it("selects the pick, else the followed or a running device, else the first free image", () => {
    const platforms = [
      platform("android", "ready", "Ready.", [PIXEL, TABLET]),
      platform("ios", "ready", "Ready.", [image("UDID-0", "Busy", true), IPHONE]),
    ];
    const android = snapshot({ deviceId: "a-1", imageId: "Tablet" });
    const ios = snapshot({ deviceId: "i-1", platform: "ios", imageId: "UDID-1" });
    const both = catalog({ platforms, devices: [android, ios] });
    expect(selectedImageOf(both, { platform: "android", imageId: "Pixel_8" }, null)).toEqual({
      platform: "android",
      imageId: "Pixel_8",
    });
    // A pick that left the catalog falls back.
    expect(selectedImageOf(both, { platform: "ios", imageId: "Gone" }, "i-1")).toEqual({
      platform: "ios",
      imageId: "UDID-1",
    });
    expect(selectedImageOf(both, null, null)).toEqual({ platform: "android", imageId: "Tablet" });
    const idle = catalog({
      platforms: [
        platform("android", "sdkMissing", "No SDK.", []),
        platform("ios", "ready", "Ready.", [image("UDID-0", "Busy", true), IPHONE]),
      ],
    });
    expect(selectedImageOf(idle, null, null)).toEqual({ platform: "ios", imageId: "UDID-1" });
  });
});
