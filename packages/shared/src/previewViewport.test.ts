import {
  PREVIEW_VIEWPORT_MAX_AREA,
  PREVIEW_VIEWPORT_MAX_DIMENSION,
  PREVIEW_VIEWPORT_MIN_DIMENSION,
  PreviewAutomationResizeInput,
} from "@coddy/contracts";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";
import { describe, expect, expectTypeOf, it } from "vite-plus/test";

import {
  PREVIEW_VIEWPORT_PRESETS,
  previewViewportLabel,
  previewViewportPresetOrientation,
  resolvePreviewViewport,
  resolveRuntimePreviewViewport,
} from "./previewViewport.ts";

const decodePreviewAutomationResizeInput = Schema.decodeUnknownOption(PreviewAutomationResizeInput);
const decodePreviewAutomationResizeInputSync = Schema.decodeUnknownSync(
  PreviewAutomationResizeInput,
);

describe("previewViewport", () => {
  it("keeps the public resolver typed while exposing runtime-boundary validation", () => {
    expectTypeOf(resolvePreviewViewport).parameter(0).toEqualTypeOf<PreviewAutomationResizeInput>();
    expectTypeOf(resolveRuntimePreviewViewport).parameter(0).toEqualTypeOf<unknown>();
  });

  it("resolves fill and exact freeform viewports", () => {
    expect(resolvePreviewViewport({ mode: "fill" })).toEqual({ _tag: "fill" });
    expect(resolvePreviewViewport({ mode: "freeform", width: 1024, height: 768 })).toEqual({
      _tag: "freeform",
      width: 1024,
      height: 768,
    });
  });

  it("resolves device presets in either orientation", () => {
    expect(resolvePreviewViewport({ mode: "preset", preset: "iphone-12-pro" })).toEqual({
      _tag: "preset",
      width: 390,
      height: 844,
      presetId: "iphone-12-pro",
    });
    expect(
      resolvePreviewViewport({
        mode: "preset",
        preset: "iphone-12-pro",
        orientation: "landscape",
      }),
    ).toEqual({
      _tag: "preset",
      width: 844,
      height: 390,
      presetId: "iphone-12-pro",
    });
  });

  it("swaps native landscape presets only when portrait is requested", () => {
    expect(resolvePreviewViewport({ mode: "preset", preset: "nest-hub" })).toEqual({
      _tag: "preset",
      width: 1024,
      height: 600,
      presetId: "nest-hub",
    });
    expect(
      resolvePreviewViewport({ mode: "preset", preset: "nest-hub", orientation: "portrait" }),
    ).toEqual({
      _tag: "preset",
      width: 600,
      height: 1024,
      presetId: "nest-hub",
    });
    expect(
      resolvePreviewViewport({
        mode: "preset",
        preset: "iphone-se",
        orientation: "portrait",
      }),
    ).toEqual({ _tag: "preset", width: 375, height: 667, presetId: "iphone-se" });
  });

  it("accepts decoded freeform boundary dimensions without changing them", () => {
    const minimum = decodePreviewAutomationResizeInputSync({
      mode: "freeform",
      width: PREVIEW_VIEWPORT_MIN_DIMENSION,
      height: PREVIEW_VIEWPORT_MIN_DIMENSION,
    });
    const maximumArea = decodePreviewAutomationResizeInputSync({
      mode: "freeform",
      width: PREVIEW_VIEWPORT_MAX_DIMENSION,
      height: 2160,
    });

    expect(resolvePreviewViewport(minimum)).toEqual({
      _tag: "freeform",
      width: 240,
      height: 240,
    });
    expect(resolvePreviewViewport(maximumArea)).toEqual({
      _tag: "freeform",
      width: 3840,
      height: 2160,
    });
  });

  it("rejects malformed, non-finite, incomplete, and out-of-range resize input at the schema boundary", () => {
    const invalidInputs = [
      { mode: "preset", preset: "unknown-device" },
      { mode: "freeform", width: 1024 },
      { mode: "freeform", width: Number.NaN, height: 768 },
      { mode: "freeform", width: Number.POSITIVE_INFINITY, height: 768 },
      { mode: "freeform", width: PREVIEW_VIEWPORT_MIN_DIMENSION - 1, height: 768 },
      { mode: "freeform", width: PREVIEW_VIEWPORT_MAX_DIMENSION + 1, height: 768 },
      { mode: "freeform", width: 3840, height: 2161 },
    ];

    for (const input of invalidInputs) {
      expect(Option.isNone(decodePreviewAutomationResizeInput(input))).toBe(true);
    }
  });

  it.each([
    ["null", null],
    ["array", []],
    ["string", "fill"],
    ["number", 1],
    ["boolean", true],
    ["undefined", undefined],
    ["function", () => undefined],
    ["custom-prototype object", Object.assign(Object.create({}), { mode: "fill" })],
  ])("rejects a non-record %s runtime input explicitly", (_label, input) => {
    expect(() => resolveRuntimePreviewViewport(input)).toThrowError(
      "Invalid preview viewport input: expected a record",
    );
  });

  it("accepts a null-prototype record", () => {
    const input = Object.assign(Object.create(null), { mode: "fill" }) as unknown;
    expect(resolveRuntimePreviewViewport(input)).toEqual({ _tag: "fill" });
  });

  it.each([
    ["missing", {}],
    ["unknown", { mode: "unknown" }],
    ["number", { mode: 1 }],
    ["null", { mode: null }],
  ])("rejects a %s viewport mode explicitly", (_label, input) => {
    expect(() => resolveRuntimePreviewViewport(input)).toThrowError(
      "Invalid preview viewport mode: expected fill, preset, or freeform",
    );
  });

  it.each([
    ["preset", { mode: "fill", preset: "iphone-se" }],
    ["width", { mode: "fill", width: 1024 }],
    ["height", { mode: "fill", height: 768 }],
    ["orientation", { mode: "fill", orientation: "landscape" }],
  ])("rejects fill mode with a %s field", (_label, input) => {
    expect(Option.isNone(decodePreviewAutomationResizeInput(input))).toBe(true);
    expect(() => resolveRuntimePreviewViewport(input)).toThrowError(
      "Fill mode does not accept a preset, dimensions, or orientation",
    );
  });

  it.each([
    ["width", { mode: "preset", preset: "iphone-se", width: 1024 }],
    ["height", { mode: "preset", preset: "iphone-se", height: 768 }],
    ["width and height", { mode: "preset", preset: "iphone-se", width: 1024, height: 768 }],
  ])("rejects preset mode with custom %s", (_label, input) => {
    expect(Option.isNone(decodePreviewAutomationResizeInput(input))).toBe(true);
    expect(() => resolveRuntimePreviewViewport(input)).toThrowError(
      "Preset mode requires a preset and does not accept custom dimensions",
    );
  });

  it.each([
    ["preset", { mode: "freeform", width: 1024, height: 768, preset: "iphone-se" }],
    ["orientation", { mode: "freeform", width: 1024, height: 768, orientation: "portrait" }],
    [
      "preset and orientation",
      {
        mode: "freeform",
        width: 1024,
        height: 768,
        preset: "iphone-se",
        orientation: "portrait",
      },
    ],
  ])("rejects freeform mode with a %s field", (_label, input) => {
    expect(Option.isNone(decodePreviewAutomationResizeInput(input))).toBe(true);
    expect(() => resolveRuntimePreviewViewport(input)).toThrowError(
      "Freeform mode requires width and height and does not accept a preset or orientation",
    );
  });

  it("rejects a missing or non-string preset and an invalid orientation", () => {
    expect(() => resolveRuntimePreviewViewport({ mode: "preset" })).toThrowError(
      "Unknown preview viewport preset: undefined",
    );
    expect(() => resolveRuntimePreviewViewport({ mode: "preset", preset: 42 })).toThrowError(
      "Unknown preview viewport preset: 42",
    );
    expect(() =>
      resolveRuntimePreviewViewport({
        mode: "preset",
        preset: "iphone-se",
        orientation: "sideways",
      }),
    ).toThrowError("Unknown preview viewport orientation: sideways");
  });

  it("rejects unknown presets received from the raw automation boundary", () => {
    expect(() =>
      resolveRuntimePreviewViewport({ mode: "preset", preset: "unknown-device" }),
    ).toThrowError("Unknown preview viewport preset: unknown-device");
  });

  it("requires both freeform dimensions at the raw automation boundary", () => {
    expect(() => resolveRuntimePreviewViewport({ mode: "freeform", height: 768 })).toThrowError(
      "Custom preview viewport requires width and height",
    );
    expect(() => resolveRuntimePreviewViewport({ mode: "freeform", width: 1024 })).toThrowError(
      "Custom preview viewport requires width and height",
    );
  });

  it.each([
    ["NaN", Number.NaN, 768],
    ["Infinity", Number.POSITIVE_INFINITY, 768],
    ["decimal", 1024.5, 768],
  ])("rejects a non-finite or non-integer %s dimension", (_label, width, height) => {
    expect(() => resolveRuntimePreviewViewport({ mode: "freeform", width, height })).toThrowError(
      "Custom preview viewport width and height must be finite integers",
    );
  });

  it.each([
    ["zero", 0, 768],
    ["negative", -1, 768],
    ["range overflow", PREVIEW_VIEWPORT_MAX_DIMENSION + 1, 768],
  ])("rejects a %s dimension outside the supported range", (_label, width, height) => {
    expect(() => resolveRuntimePreviewViewport({ mode: "freeform", width, height })).toThrowError(
      `Custom preview viewport dimensions must be between ${PREVIEW_VIEWPORT_MIN_DIMENSION} and ${PREVIEW_VIEWPORT_MAX_DIMENSION}`,
    );
  });

  it("rejects a viewport whose dimensions exceed the maximum area", () => {
    expect(() =>
      resolveRuntimePreviewViewport({ mode: "freeform", width: 3840, height: 2161 }),
    ).toThrowError(
      `Custom preview viewport area must not exceed ${PREVIEW_VIEWPORT_MAX_AREA} pixels`,
    );
  });

  it("matches Chrome's standard device catalog ordering", () => {
    expect(PREVIEW_VIEWPORT_PRESETS.map((preset) => preset.label)).toEqual([
      "iPhone SE",
      "iPhone XR",
      "iPhone 12 Pro",
      "iPhone 14 Pro Max",
      "Pixel 7",
      "Samsung Galaxy S8+",
      "Samsung Galaxy S20 Ultra",
      "iPad Mini",
      "iPad Air",
      "iPad Pro",
      "Surface Pro 7",
      "Surface Duo",
      "Galaxy Z Fold 5",
      "Asus Zenbook Fold",
      "Samsung Galaxy A51/71",
      "Nest Hub",
      "Nest Hub Max",
    ]);
  });

  it("formats settings for compact UI", () => {
    expect(previewViewportLabel({ _tag: "fill" })).toBe("Fill panel");
    expect(previewViewportLabel({ _tag: "freeform", width: 393, height: 852 })).toBe("393 × 852");
    expect(previewViewportPresetOrientation({ _tag: "freeform", width: 852, height: 393 })).toBe(
      "landscape",
    );
    expect(previewViewportPresetOrientation({ _tag: "freeform", width: 393, height: 852 })).toBe(
      "portrait",
    );
    expect(
      previewViewportPresetOrientation({ _tag: "freeform", width: 500, height: 500 }),
    ).toBeNull();
    expect(previewViewportPresetOrientation({ _tag: "fill" })).toBeNull();
  });
});
