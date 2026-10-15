import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";
import {
  DeviceCatalog,
  DeviceInputInput,
  DeviceJpegFrame,
  DeviceRotateInput,
  DeviceRpcError,
} from "./device.ts";

describe("device preview contracts", () => {
  it("decodes a catalog as the server serializes it", () => {
    const catalog = {
      availability: { kind: "ready", message: "Ready." },
      avds: [{ name: "Pixel_8", displayName: "Pixel 8" }],
      devices: [
        // A device that has not reported its display omits failure, width and height.
        { deviceId: "d-1", avdName: "Pixel_8", state: "starting", orientation: "portrait" },
        {
          deviceId: "d-2",
          avdName: "Pixel_8",
          state: "failed",
          failure: "exited: 1",
          width: 2400,
          height: 1080,
          orientation: "reverseLandscape",
        },
      ],
    };
    expect(Schema.decodeUnknownSync(DeviceCatalog)(catalog)).toEqual(catalog);
    expect(
      Schema.is(DeviceCatalog)({
        ...catalog,
        availability: { kind: "sdkMissing", message: "Looking for the Android SDK on the server." },
      }),
    ).toBe(true);
    expect(
      Schema.is(DeviceCatalog)({
        ...catalog,
        devices: [{ ...catalog.devices[0], width: null }],
      }),
    ).toBe(false);
  });

  it("bounds input batches and text", () => {
    const touch = { type: "touch", action: "down", pointerId: 0, x: 0.5, y: 0.25 };
    const input = {
      deviceId: "d-1",
      events: [
        touch,
        { type: "key", key: "appSwitch", action: "up" },
        { type: "text", text: "hi" },
      ],
    };
    expect(Schema.decodeUnknownSync(DeviceInputInput)(input)).toEqual(input);
    expect(Schema.is(DeviceInputInput)({ deviceId: "d-1", events: [] })).toBe(false);
    expect(
      Schema.is(DeviceInputInput)({
        deviceId: "d-1",
        events: Array.from({ length: 64 }, () => touch),
      }),
    ).toBe(true);
    expect(
      Schema.is(DeviceInputInput)({
        deviceId: "d-1",
        events: Array.from({ length: 65 }, () => touch),
      }),
    ).toBe(false);
    const text = (length: number) => ({
      deviceId: "d-1",
      events: [{ type: "text", text: "a".repeat(length) }],
    });
    expect(Schema.is(DeviceInputInput)(text(1000))).toBe(true);
    expect(Schema.is(DeviceInputInput)(text(1001))).toBe(false);
    expect(
      Schema.is(DeviceInputInput)({
        deviceId: "d-1",
        events: [{ type: "key", key: "escape", action: "down" }],
      }),
    ).toBe(false);
  });

  it("decodes rotation requests and frames", () => {
    expect(
      Schema.decodeUnknownSync(DeviceRotateInput)({ deviceId: "d-1", orientation: "landscape" }),
    ).toEqual({ deviceId: "d-1", orientation: "landscape" });
    expect(Schema.is(DeviceRotateInput)({ deviceId: "d-1", orientation: "upsideDown" })).toBe(
      false,
    );
    const frame = { seq: 3, width: 720, height: 1600, orientation: "portrait", jpegBase64: "/9j/" };
    expect(Schema.decodeUnknownSync(DeviceJpegFrame)(frame)).toEqual(frame);
  });

  it("decodes each tagged error", () => {
    const errors = [
      { _tag: "DevicePreviewsDisabledError", message: "Device previews are off." },
      { _tag: "DeviceSdkUnavailableError", kind: "noAvds", message: "Create a virtual device." },
      { _tag: "DeviceLimitError", running: "Pixel_8", message: "Pixel_8 is already running." },
      { _tag: "DeviceNotFoundError", message: "This device is no longer running." },
      { _tag: "DeviceNotReadyError", state: "booting", message: "The device is still starting." },
      { _tag: "DeviceInputError", message: "Send at least one input event." },
      { _tag: "DeviceEmulatorError", message: "The emulator did not respond." },
      { _tag: "DeviceMediaUnavailableError", message: "Video needs a signed-in session." },
    ];
    const decode = Schema.decodeUnknownSync(DeviceRpcError);
    for (const error of errors) {
      expect(decode(error)).toMatchObject(error);
    }
    expect(() => decode({ _tag: "DeviceNotReadyError", state: "asleep", message: "x" })).toThrow();
    expect(() => decode({ _tag: "DeviceEmulatorError" })).toThrow();
  });
});
