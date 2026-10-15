import type { DeviceOrientation } from "@bibcode/contracts";

/** Where the device screen sits inside its container, in container pixels. */
export interface Letterbox {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

const EMPTY_BOX: Letterbox = { x: 0, y: 0, width: 0, height: 0 };

/** The largest box with the device's aspect ratio that fits the container, centered. */
export function letterbox(
  containerW: number,
  containerH: number,
  deviceW: number,
  deviceH: number,
): Letterbox {
  if (!(containerW > 0 && containerH > 0 && deviceW > 0 && deviceH > 0)) return EMPTY_BOX;
  // Multiply before dividing so exact ratios stay exact.
  if (containerW * deviceH > containerH * deviceW) {
    const width = (containerH * deviceW) / deviceH;
    return { x: (containerW - width) / 2, y: 0, width, height: containerH };
  }
  const height = (containerW * deviceH) / deviceW;
  return { x: 0, y: (containerH - height) / 2, width: containerW, height };
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
 * positions are normalized to that orientation, so this is the reported size. If the emulator
 * turns out to send native-size frames plus a rotation (native verification item 3), this is
 * the one function that swaps for `landscape` and `reverseLandscape`.
 */
export function displaySize(
  width: number,
  height: number,
  _orientation: DeviceOrientation,
): { width: number; height: number } {
  return { width, height };
}
