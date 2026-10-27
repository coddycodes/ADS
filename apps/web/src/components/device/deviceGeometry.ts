import type { DeviceOrientation } from "@coddy/contracts";

/** Where the device screen sits inside its container, in container pixels. */
export interface Letterbox {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

const EMPTY_BOX: Letterbox = { x: 0, y: 0, width: 0, height: 0 };

/**
 * The largest box with the device's aspect ratio that fits the container shrunk by `inset` on
 * every side, centered in the container. The box stays in container coordinates; an inset
 * that leaves no room gives the empty box.
 */
export function letterbox(
  containerW: number,
  containerH: number,
  deviceW: number,
  deviceH: number,
  inset = 0,
): Letterbox {
  const availableW = containerW - 2 * inset;
  const availableH = containerH - 2 * inset;
  if (!(availableW > 0 && availableH > 0 && deviceW > 0 && deviceH > 0)) return EMPTY_BOX;
  // Multiply before dividing so exact ratios stay exact.
  if (availableW * deviceH > availableH * deviceW) {
    const width = (availableH * deviceW) / deviceH;
    return { x: (containerW - width) / 2, y: inset, width, height: availableH };
  }
  const height = (availableW * deviceH) / deviceW;
  return { x: inset, y: (containerH - height) / 2, width: availableW, height };
}

/**
 * A container-relative point as `0..1` of the device screen, or `null` when it falls on the
 * bars around it.
 */
export function toNormalized(
  clientX: number,
  clientY: number,
  box: Letterbox,
): { x: number; y: number } | null {
  if (box.width <= 0 || box.height <= 0) return null;
  const x = (clientX - box.x) / box.width;
  const y = (clientY - box.y) / box.height;
  if (x < 0 || x > 1 || y < 0 || y > 1) return null;
  return { x, y };
}

/** Like `toNormalized`, but a point off the screen lands on its nearest edge. */
export function clampToBox(
  clientX: number,
  clientY: number,
  box: Letterbox,
): { x: number; y: number } {
  if (box.width <= 0 || box.height <= 0) return { x: 0, y: 0 };
  const clamp = (value: number) => Math.min(1, Math.max(0, value));
  return {
    x: clamp((clientX - box.x) / box.width),
    y: clamp((clientY - box.y) / box.height),
  };
}

/**
 * The size the device screen shows at, from a size the server reported with its orientation.
 *
 * The server reports frame and snapshot sizes already in the current orientation, and touch
 * positions are normalized to that upright picture, so this is the reported size. The first
 * native Android run confirmed it (native verification item 3): after a turn to landscape the
 * emulator's frames arrived oriented (1280x576 for a 576x1280 portrait stream), not at the
 * native size plus a rotation. The server maps touches back to the unrotated panel itself.
 */
export function displaySize(
  width: number,
  height: number,
  _orientation: DeviceOrientation,
): { width: number; height: number } {
  return { width, height };
}
