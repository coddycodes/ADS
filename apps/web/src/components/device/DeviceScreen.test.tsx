// @vitest-environment happy-dom

import type { DeviceInputEvent, DeviceSnapshot } from "@coddy/contracts";
import { EnvironmentId } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const atoms = vi.hoisted(() => ({
  subscribed: [] as unknown[],
  videoMode: { kind: "jpeg", reason: "unsupported" } as VideoMode,
  restarts: [] as number[],
  stillImages: [] as Array<{ reason: string; message: string } | null>,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomSubscribe: (atom: unknown) => {
    atoms.subscribed.push(atom);
  },
  useAtomRefresh: () => () => undefined,
}));

vi.mock("./useDeviceVideo", () => ({
  useDeviceVideo: (input: { restart?: number }) => {
    atoms.restarts.push(input.restart ?? 0);
    return atoms.videoMode;
  },
}));

vi.mock("../../state/device", () => ({
  deviceEnvironment: { frames: (args: unknown) => ({ __q: "frames", args }) },
}));

import { DeviceInputQueue } from "./deviceInputQueue";
import { DeviceScreen } from "./DeviceScreen";
import type { VideoMode } from "./useDeviceVideo";

const ENVIRONMENT_ID = EnvironmentId.make("environment-work");
const device: DeviceSnapshot = {
  deviceId: "d-1",
  platform: "android",
  imageId: "Pixel_8",
  state: "ready",
  orientation: "portrait",
  width: 1080,
  height: 2400,
  canRotate: true,
};

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let sent: DeviceInputEvent[] = [];
let queue: DeviceInputQueue;

async function render(props: { inputDisabledReason?: string | null } = {}) {
  queue = new DeviceInputQueue(
    (events) => {
      sent.push(...events);
      return Promise.resolve();
    },
    (flush) => flush(),
  );
  const onRelease = (scope: "keys" | "all") =>
    scope === "keys" ? queue.releaseKeys() : queue.releaseAll();
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => {
    root!.render(
      <DeviceScreen
        environmentId={ENVIRONMENT_ID}
        device={device}
        name="Pixel 8"
        inputDisabledReason={props.inputDisabledReason ?? null}
        onInput={(event) => queue.push(event)}
        onRelease={onRelease}
        onStillImages={(still) => atoms.stillImages.push(still)}
      />,
    );
  });
  return container.querySelector("canvas")!;
}

const settle = () =>
  act(async () => {
    for (let tick = 0; tick < 5; tick += 1) await Promise.resolve();
  });

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  sent = [];
  atoms.subscribed = [];
  atoms.restarts = [];
  atoms.stillImages = [];
  atoms.videoMode = { kind: "jpeg", reason: "unsupported" };
  // A 1000×800 screen area, so a portrait device sits at x 320..680.
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(1000);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(800);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
    left: 0,
    top: 0,
    width: 1000,
    height: 800,
  } as DOMRect);
  HTMLCanvasElement.prototype.setPointerCapture = vi.fn();
});

afterEach(async () => {
  if (root) await act(async () => root!.unmount());
  container?.remove();
  root = null;
  vi.restoreAllMocks();
});

describe("DeviceScreen", () => {
  it("subscribes to JPEG frames only in JPEG mode", async () => {
    atoms.videoMode = { kind: "h264", stalled: false };
    await render();
    expect(atoms.subscribed).toEqual([]);
    expect(container!.textContent).not.toContain("Waiting for the screen");
    expect(container!.textContent).not.toContain("stopped updating");
  });

  it("offers to reconnect when video stalls, restarting it", async () => {
    atoms.videoMode = { kind: "h264", stalled: true };
    await render();
    expect(container!.textContent).toContain("The screen stopped updating.");
    const restartsBefore = atoms.restarts.at(-1);
    const reconnect = [...container!.querySelectorAll("button")].find(
      (button) => button.textContent === "Reconnect screen",
    )!;
    await act(async () => reconnect.click());
    expect(atoms.restarts.at(-1)).toBe((restartsBefore ?? 0) + 1);
  });

  it("lets touches reach the screen under the stall notice; only its button takes the pointer", async () => {
    atoms.videoMode = { kind: "h264", stalled: true };
    const canvas = await render();
    const notice = container!.querySelector<HTMLElement>("[data-screen-stalled]")!;
    expect(notice.textContent).toContain("The screen stopped updating.");
    // happy-dom does no hit testing, so the pass-through is the overlay's pointer-events.
    expect(notice.classList).toContain("pointer-events-none");
    const reconnect = notice.querySelector("button")!;
    expect(reconnect.classList).toContain("pointer-events-auto");
    for (const element of notice.querySelectorAll<HTMLElement>("*")) {
      if (element !== reconnect && !reconnect.contains(element)) {
        expect(element.classList).not.toContain("pointer-events-auto");
      }
    }
    await act(async () => {
      canvas.dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          pointerId: 1,
          pointerType: "mouse",
          button: 0,
          clientX: 500,
          clientY: 400,
        }),
      );
    });
    await settle();
    expect(sent).toContainEqual({ type: "touch", action: "down", pointerId: 0, x: 0.5, y: 0.5 });
  });

  it("waits for the screen while video is starting", async () => {
    atoms.videoMode = { kind: "probing" };
    await render();
    expect(atoms.subscribed).toEqual([]);
    expect(container!.textContent).toContain("Waiting for the screen");
  });

  it("reports the still-images reason to its panel and draws nothing over the screen", async () => {
    atoms.videoMode = { kind: "jpeg", reason: "decoderError" };
    await render();
    expect(atoms.subscribed).not.toEqual([]);
    expect(atoms.stillImages.at(-1)).toEqual({
      reason: "decoderError",
      message:
        "Video could not be decoded, so the screen updates as still images. Reopen the device panel to try video again.",
    });
    expect(container!.querySelector("[data-still-images]")).toBeNull();
    await act(async () => root!.unmount());
    root = null;
    expect(atoms.stillImages.at(-1)).toBeNull();
  });

  it("lifts a held key when the screen loses focus", async () => {
    const canvas = await render();
    await act(async () => {
      canvas.dispatchEvent(new KeyboardEvent("keydown", { key: "Backspace", bubbles: true }));
    });
    await settle();
    expect(sent).toEqual([{ type: "key", key: "backspace", action: "down" }]);
    await act(async () => {
      canvas.dispatchEvent(new FocusEvent("focusout", { bubbles: true }));
    });
    await settle();
    expect(sent.at(-1)).toEqual({ type: "key", key: "backspace", action: "up" });
  });

  it("releases a held key even when Ctrl joins before the key comes up", async () => {
    const canvas = await render();
    const down = new KeyboardEvent("keydown", {
      key: "Backspace",
      bubbles: true,
      cancelable: true,
    });
    const up = new KeyboardEvent("keyup", {
      key: "Backspace",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    await act(async () => {
      canvas.dispatchEvent(down);
      canvas.dispatchEvent(up);
    });
    await settle();
    expect(sent).toEqual([
      { type: "key", key: "backspace", action: "down" },
      { type: "key", key: "backspace", action: "up" },
    ]);
    // The press was the screen's, the Ctrl release is not: browser shortcuts see it as before.
    expect(down.defaultPrevented).toBe(true);
    expect(up.defaultPrevented).toBe(false);
  });

  it("sends nothing for Ctrl+Backspace, press or release, and leaves both to the app", async () => {
    const canvas = await render();
    const down = new KeyboardEvent("keydown", {
      key: "Backspace",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    const up = new KeyboardEvent("keyup", {
      key: "Backspace",
      ctrlKey: true,
      bubbles: true,
      cancelable: true,
    });
    await act(async () => {
      canvas.dispatchEvent(down);
      canvas.dispatchEvent(up);
    });
    await settle();
    expect(sent).toEqual([]);
    expect(down.defaultPrevented).toBe(false);
    expect(up.defaultPrevented).toBe(false);
  });

  it("takes a plain key press and its release from the browser", async () => {
    const canvas = await render();
    const down = new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
    const up = new KeyboardEvent("keyup", { key: "Enter", bubbles: true, cancelable: true });
    await act(async () => {
      canvas.dispatchEvent(down);
      canvas.dispatchEvent(up);
    });
    await settle();
    expect(sent).toEqual([
      { type: "key", key: "enter", action: "down" },
      { type: "key", key: "enter", action: "up" },
    ]);
    expect(down.defaultPrevented).toBe(true);
    expect(up.defaultPrevented).toBe(true);
  });

  it("lifts a held key when the window loses focus", async () => {
    const canvas = await render();
    await act(async () => {
      canvas.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowUp", bubbles: true }));
      window.dispatchEvent(new Event("blur"));
    });
    await settle();
    expect(sent).toEqual([
      { type: "key", key: "arrowUp", action: "down" },
      { type: "key", key: "arrowUp", action: "up" },
    ]);
  });

  it("releases held keys and touches when the page is hidden", async () => {
    const canvas = await render();
    await act(async () => {
      canvas.dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          pointerId: 3,
          pointerType: "touch",
          clientX: 500,
          clientY: 400,
        }),
      );
      canvas.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    });
    await settle();
    vi.spyOn(document, "hidden", "get").mockReturnValue(true);
    await act(async () => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await settle();
    expect(sent.slice(-2)).toEqual([
      { type: "key", key: "enter", action: "up" },
      { type: "touch", action: "cancel", pointerId: 0, x: 0.5, y: 0.5 },
    ]);
  });

  it("cancels a drag in progress when it unmounts", async () => {
    const canvas = await render();
    await act(async () => {
      canvas.dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          pointerId: 7,
          pointerType: "touch",
          clientX: 500,
          clientY: 400,
        }),
      );
    });
    await settle();
    expect(sent).toEqual([{ type: "touch", action: "down", pointerId: 0, x: 0.5, y: 0.5 }]);
    await act(async () => root!.unmount());
    root = null;
    await settle();
    expect(sent.at(-1)).toEqual({ type: "touch", action: "cancel", pointerId: 0, x: 0.5, y: 0.5 });
  });

  it("passes on the server's own reason for still images", async () => {
    atoms.videoMode = { kind: "jpeg", reason: "noVideo", message: "Video is off: view only." };
    await render();
    expect(atoms.stillImages.at(-1)).toEqual({
      reason: "noVideo",
      message: "Video is off: view only.",
    });
  });

  it("names the device, not its image id, to screen readers", async () => {
    const canvas = await render();
    expect(canvas.getAttribute("aria-label")).toContain("Pixel 8 screen.");
    expect(canvas.getAttribute("aria-label")).not.toContain("Pixel_8");
  });

  it("view-only shows why above the screen and sends no input", async () => {
    const reason =
      "Live input needs someone logged in on this Mac. Simulators open as screenshots.";
    const canvas = await render({ inputDisabledReason: reason });
    const notice = container!.querySelector("[data-view-only]");
    expect(notice?.textContent).toBe(reason);
    // The notice sits above the screen, outside the canvas's box.
    expect(notice?.compareDocumentPosition(canvas)).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(canvas.style.cursor).toBe("default");
    expect(canvas.getAttribute("aria-label")).toContain("view only");
    await act(async () => {
      canvas.dispatchEvent(
        new PointerEvent("pointerdown", {
          bubbles: true,
          pointerId: 1,
          pointerType: "mouse",
          button: 0,
          clientX: 500,
          clientY: 400,
        }),
      );
      canvas.dispatchEvent(
        new PointerEvent("pointerup", { bubbles: true, pointerId: 1, clientX: 500, clientY: 400 }),
      );
      canvas.dispatchEvent(new KeyboardEvent("keydown", { key: "a", bubbles: true }));
      const paste = new Event("paste", { bubbles: true, cancelable: true });
      Object.assign(paste, { clipboardData: { getData: () => "hello" } });
      canvas.dispatchEvent(paste);
    });
    await settle();
    expect(sent).toEqual([]);
  });
});
