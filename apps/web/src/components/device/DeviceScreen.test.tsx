// @vitest-environment happy-dom

import type { DeviceInputEvent, DeviceSnapshot } from "@bibcode/contracts";
import { EnvironmentId } from "@bibcode/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("@effect/atom-react", () => ({
  useAtomSubscribe: () => undefined,
  useAtomRefresh: () => () => undefined,
}));

vi.mock("../../state/device", () => ({
  deviceEnvironment: { frames: (args: unknown) => ({ __q: "frames", args }) },
}));

import { DeviceInputQueue } from "./deviceInputQueue";
import { DeviceScreen } from "./DeviceScreen";

const ENVIRONMENT_ID = EnvironmentId.make("environment-work");
const device: DeviceSnapshot = {
  deviceId: "d-1",
  avdName: "Pixel_8",
  state: "ready",
  orientation: "portrait",
  width: 1080,
  height: 2400,
};

let root: Root | null = null;
let container: HTMLDivElement | null = null;
let sent: DeviceInputEvent[] = [];
let queue: DeviceInputQueue;

async function render() {
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
        onInput={(event) => queue.push(event)}
        onRelease={onRelease}
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
});
