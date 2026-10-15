import { describe, expect, it } from "vite-plus/test";

import { clampToBox, displaySize, letterbox, toNormalized } from "./deviceGeometry";

describe("deviceGeometry", () => {
  it("letterboxes a portrait device in a wide container", () => {
    expect(letterbox(1000, 800, 1080, 2400)).toEqual({ x: 320, y: 0, width: 360, height: 800 });
  });

  it("letterboxes a landscape device in a tall container", () => {
    expect(letterbox(400, 1000, 2400, 1080)).toEqual({ x: 0, y: 410, width: 400, height: 180 });
  });

  it("gives an empty box when either size is unknown", () => {
    expect(letterbox(0, 800, 1080, 2400)).toEqual({ x: 0, y: 0, width: 0, height: 0 });
    expect(letterbox(1000, 800, 0, 2400)).toEqual({ x: 0, y: 0, width: 0, height: 0 });
  });

  it("maps client points to normalized coordinates and rejects the bars", () => {
    const box = letterbox(1000, 800, 1080, 2400);
    expect(toNormalized(500, 400, box)).toEqual({ x: 0.5, y: 0.5 });
    expect(toNormalized(100, 400, box)).toBeNull();
    expect(toNormalized(900, 400, box)).toBeNull();
    expect(toNormalized(500, 400, { x: 0, y: 0, width: 0, height: 0 })).toBeNull();
  });

  it("clamps a captured drag that leaves the screen to its edge", () => {
    const box = letterbox(1000, 800, 1080, 2400);
    expect(clampToBox(100, 900, box)).toEqual({ x: 0, y: 1 });
    expect(clampToBox(500, 400, box)).toEqual({ x: 0.5, y: 0.5 });
  });

  // The server reports sizes already in the current orientation (native verification item 3),
  // so the display size is the reported one. This is the one place to change if it is not.
  it("keeps the reported size in every orientation", () => {
    expect(displaySize(1080, 2400, "portrait")).toEqual({ width: 1080, height: 2400 });
    expect(displaySize(2400, 1080, "landscape")).toEqual({ width: 2400, height: 1080 });
    expect(displaySize(2400, 1080, "reverseLandscape")).toEqual({ width: 2400, height: 1080 });
  });
});
