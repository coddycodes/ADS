import { describe, expect, it } from "@effect/vitest";
import * as Schema from "effect/Schema";
import {
  DeviceCatalog,
  DeviceImage,
  DeviceInputInput,
  DeviceJpegFrame,
  DeviceOpenInput,
  DeviceRotateInput,
  DeviceRotateResult,
  DeviceRpcError,
  DeviceSnapshot,
} from "./device.ts";

const decodeCatalog = Schema.decodeUnknownSync(DeviceCatalog);
const isCatalog = Schema.is(DeviceCatalog);
const decodeImage = Schema.decodeUnknownSync(DeviceImage);
const isImage = Schema.is(DeviceImage);
const isSnapshot = Schema.is(DeviceSnapshot);
const decodeOpen = Schema.decodeUnknownSync(DeviceOpenInput);
const isOpen = Schema.is(DeviceOpenInput);
const decodeInput = Schema.decodeUnknownSync(DeviceInputInput);
const isInput = Schema.is(DeviceInputInput);
const decodeRotate = Schema.decodeUnknownSync(DeviceRotateInput);
const isRotate = Schema.is(DeviceRotateInput);
const decodeRotated = Schema.decodeUnknownSync(DeviceRotateResult);
const isRotated = Schema.is(DeviceRotateResult);
const decodeFrame = Schema.decodeUnknownSync(DeviceJpegFrame);
const decodeError = Schema.decodeUnknownSync(DeviceRpcError);

describe("device preview contracts", () => {
  it("decodes a two-platform catalog as the server serializes it", () => {
    const catalog = {
      platforms: [
        {
          platform: "android",
          availability: { kind: "ready", message: "Ready." },
          images: [{ id: "Pixel_8", displayName: "Pixel 8" }],
        },
        {
          platform: "ios",
          availability: { kind: "viewOnlyHeadless", message: "Touch input is off." },
          images: [
            { id: "A1B2", displayName: "iPhone 17 (iOS 27.0)", inUseElsewhere: true },
            { id: "C3D4", displayName: "iPad Air (iOS 27.0)" },
          ],
        },
      ],
      devices: [
        // A device that has not reported its display omits failure, width and height.
        {
          deviceId: "d-1",
          platform: "android",
          imageId: "Pixel_8",
          state: "starting",
          orientation: "portrait",
          canRotate: false,
        },
        {
          deviceId: "d-2",
          platform: "ios",
          imageId: "C3D4",
          state: "failed",
          failure: "exited: 1",
          width: 2400,
          height: 1080,
          orientation: "reverseLandscape",
          canRotate: true,
        },
      ],
    };
    expect(decodeCatalog(catalog)).toEqual(catalog);
    expect(
      isCatalog({
        ...catalog,
        devices: [{ ...catalog.devices[0], width: null }],
      }),
    ).toBe(false);
  });

  it("marks only images running elsewhere, and only with true", () => {
    const image = { id: "A1B2", displayName: "iPhone 17" };
    expect(decodeImage(image)).toEqual(image);
    expect(decodeImage({ ...image, inUseElsewhere: true })).toEqual({
      ...image,
      inUseElsewhere: true,
    });
    expect(isImage({ ...image, inUseElsewhere: false })).toBe(false);
  });

  it("requires canRotate and platform on every device", () => {
    const device = {
      deviceId: "d-1",
      platform: "android",
      imageId: "Pixel_8",
      state: "ready",
      orientation: "portrait",
      canRotate: true,
    };
    expect(isSnapshot(device)).toBe(true);
    const { canRotate: _canRotate, ...withoutRotate } = device;
    expect(isSnapshot(withoutRotate)).toBe(false);
    const { platform: _platform, ...withoutPlatform } = device;
    expect(isSnapshot(withoutPlatform)).toBe(false);
    expect(isSnapshot({ ...device, platform: "windows" })).toBe(false);
  });

  it("rejects the Android-only catalog shape", () => {
    expect(
      isCatalog({
        platforms: [{ availability: { kind: "ready", message: "Ready." }, images: [] }],
        devices: [],
      }),
    ).toBe(false);
    expect(
      isCatalog({
        availability: { kind: "ready", message: "Ready." },
        avds: [{ name: "Pixel_8", displayName: "Pixel 8" }],
        devices: [],
      }),
    ).toBe(false);
  });

  it("opens an image on a platform", () => {
    expect(decodeOpen({ platform: "ios", imageId: "A1B2" })).toEqual({
      platform: "ios",
      imageId: "A1B2",
    });
    expect(isOpen({ imageId: "A1B2" })).toBe(false);
    expect(isOpen({ avdName: "Pixel_8" })).toBe(false);
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
    expect(decodeInput(input)).toEqual(input);
    expect(isInput({ deviceId: "d-1", events: [] })).toBe(false);
    expect(
      isInput({
        deviceId: "d-1",
        events: Array.from({ length: 64 }, () => touch),
      }),
    ).toBe(true);
    expect(
      isInput({
        deviceId: "d-1",
        events: Array.from({ length: 65 }, () => touch),
      }),
    ).toBe(false);
    const text = (length: number) => ({
      deviceId: "d-1",
      events: [{ type: "text", text: "a".repeat(length) }],
    });
    expect(isInput(text(1000))).toBe(true);
    expect(isInput(text(1001))).toBe(false);
    expect(
      isInput({
        deviceId: "d-1",
        events: [{ type: "key", key: "escape", action: "down" }],
      }),
    ).toBe(false);
  });

  it("decodes rotation requests and frames", () => {
    expect(decodeRotate({ deviceId: "d-1", orientation: "landscape" })).toEqual({
      deviceId: "d-1",
      orientation: "landscape",
    });
    expect(isRotate({ deviceId: "d-1", orientation: "upsideDown" })).toBe(false);
    // The orientation the device ended in, as the server serializes it.
    expect(decodeRotated({ orientation: "portrait" })).toEqual({ orientation: "portrait" });
    expect(isRotated({})).toBe(false);
    expect(isRotated(null)).toBe(false);
    expect(isRotated({ orientation: "upsideDown" })).toBe(false);
    const frame = { seq: 3, width: 720, height: 1600, orientation: "portrait", jpegBase64: "/9j/" };
    expect(decodeFrame(frame)).toEqual(frame);
  });

  it("decodes each tagged error", () => {
    const errors = [
      { _tag: "DevicePreviewsDisabledError", message: "Device previews are off." },
      { _tag: "DeviceSdkUnavailableError", kind: "noAvds", message: "Create a virtual device." },
      { _tag: "DeviceLimitError", running: "Pixel 8", message: "Pixel 8 is already running." },
      {
        _tag: "DeviceBusyError",
        imageId: "A1B2",
        message:
          "This simulator is already running outside BiBCode. Shut it down in Simulator, then open it here.",
      },
      { _tag: "DeviceSdkUnavailableError", kind: "xcodeMissing", message: "Install Xcode." },
      { _tag: "DeviceNotFoundError", message: "This device is no longer running." },
      { _tag: "DeviceNotReadyError", state: "booting", message: "The device is still starting." },
      { _tag: "DeviceInputError", message: "Send at least one input event." },
      { _tag: "DeviceEmulatorError", message: "The emulator did not respond." },
      { _tag: "DeviceMediaUnavailableError", message: "Video needs a signed-in session." },
    ];
    for (const error of errors) {
      expect(decodeError(error)).toMatchObject(error);
    }
    expect(() =>
      decodeError({ _tag: "DeviceNotReadyError", state: "asleep", message: "x" }),
    ).toThrow();
    expect(() => decodeError({ _tag: "DeviceEmulatorError" })).toThrow();
    expect(() => decodeError({ _tag: "DeviceBusyError", message: "x" })).toThrow();
  });
});
