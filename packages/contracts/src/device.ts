import * as Schema from "effect/Schema";

/** The platform a device image runs. */
export const DevicePlatform = Schema.Literals(["android", "ios"]);
export type DevicePlatform = typeof DevicePlatform.Type;

/**
 * Why a platform can or cannot run devices. `sdkMissing` through `accelerationUnavailable`
 * are Android's; `xcodeMissing` through `viewOnlyHeadless` are iOS's.
 */
export const DeviceAvailabilityKind = Schema.Literals([
  "ready",
  "sdkMissing",
  "adbMissing",
  "noAvds",
  "accelerationUnavailable",
  "xcodeMissing",
  "xcodeSetupIncomplete",
  "noSimulators",
  "xcodeUnsupported",
  "viewOnlyHeadless",
]);
export type DeviceAvailabilityKind = typeof DeviceAvailabilityKind.Type;

export const DeviceState = Schema.Literals([
  "starting",
  "booting",
  "ready",
  "stopping",
  "stopped",
  "failed",
]);
export type DeviceState = typeof DeviceState.Type;

export const DeviceOrientation = Schema.Literals([
  "portrait",
  "landscape",
  "reversePortrait",
  "reverseLandscape",
]);
export type DeviceOrientation = typeof DeviceOrientation.Type;

/** A device a platform can start: an Android AVD or an iOS simulator. */
export const DeviceImage = Schema.Struct({
  /** The AVD name or the simulator UDID. */
  id: Schema.String,
  displayName: Schema.String,
  /** Present, and only ever `true`, when the image already runs outside BiBCode. */
  inUseElsewhere: Schema.optionalKey(Schema.Literal(true)),
});
export type DeviceImage = typeof DeviceImage.Type;

/** One platform's availability and the images it can start. */
export const DevicePlatformCatalog = Schema.Struct({
  platform: DevicePlatform,
  availability: Schema.Struct({ kind: DeviceAvailabilityKind, message: Schema.String }),
  images: Schema.Array(DeviceImage),
});
export type DevicePlatformCatalog = typeof DevicePlatformCatalog.Type;

/**
 * `width` and `height` are the display size in the current orientation. `canRotate` is whether
 * the running device turns.
 */
export const DeviceSnapshot = Schema.Struct({
  deviceId: Schema.String,
  platform: DevicePlatform,
  imageId: Schema.String,
  state: DeviceState,
  failure: Schema.optionalKey(Schema.String),
  width: Schema.optionalKey(Schema.Number),
  height: Schema.optionalKey(Schema.Number),
  orientation: DeviceOrientation,
  canRotate: Schema.Boolean,
});
export type DeviceSnapshot = typeof DeviceSnapshot.Type;

/** One entry per platform the server runs, in its order, and every device it knows. */
export const DeviceCatalog = Schema.Struct({
  platforms: Schema.Array(DevicePlatformCatalog),
  devices: Schema.Array(DeviceSnapshot),
});
export type DeviceCatalog = typeof DeviceCatalog.Type;

export const DeviceKey = Schema.Literals([
  "back",
  "home",
  "appSwitch",
  "power",
  "volumeUp",
  "volumeDown",
  "enter",
  "backspace",
  "delete",
  "tab",
  "arrowUp",
  "arrowDown",
  "arrowLeft",
  "arrowRight",
]);
export type DeviceKey = typeof DeviceKey.Type;

/** Touch `x` and `y` are normalized to `0..1` in the device's current orientation. */
export const DeviceInputEvent = Schema.Union([
  Schema.Struct({
    type: Schema.Literal("touch"),
    action: Schema.Literals(["down", "move", "up", "cancel"]),
    pointerId: Schema.Number,
    x: Schema.Number,
    y: Schema.Number,
  }),
  Schema.Struct({
    type: Schema.Literal("key"),
    key: DeviceKey,
    action: Schema.Literals(["down", "up"]),
  }),
  Schema.Struct({
    type: Schema.Literal("text"),
    text: Schema.String.check(Schema.isMaxLength(1000)),
  }),
]);
export type DeviceInputEvent = typeof DeviceInputEvent.Type;

export const DeviceOpenInput = Schema.Struct({ platform: DevicePlatform, imageId: Schema.String });
export type DeviceOpenInput = typeof DeviceOpenInput.Type;
export const DeviceOpenResult = Schema.Struct({ deviceId: Schema.String });
export type DeviceOpenResult = typeof DeviceOpenResult.Type;
export const DeviceIdInput = Schema.Struct({ deviceId: Schema.String });
export type DeviceIdInput = typeof DeviceIdInput.Type;
export const DeviceInputInput = Schema.Struct({
  deviceId: Schema.String,
  events: Schema.Array(DeviceInputEvent).check(Schema.isMinLength(1), Schema.isMaxLength(64)),
});
export type DeviceInputInput = typeof DeviceInputInput.Type;
export const DeviceRotateInput = Schema.Struct({
  deviceId: Schema.String,
  orientation: DeviceOrientation,
});
export type DeviceRotateInput = typeof DeviceRotateInput.Type;

export const DeviceJpegFrame = Schema.Struct({
  seq: Schema.Number,
  width: Schema.Number,
  height: Schema.Number,
  orientation: DeviceOrientation,
  jpegBase64: Schema.String,
});
export type DeviceJpegFrame = typeof DeviceJpegFrame.Type;

export const DeviceMediaTicket = Schema.Struct({
  ticket: Schema.String,
  expiresAtMs: Schema.Number,
});
export type DeviceMediaTicket = typeof DeviceMediaTicket.Type;

export class DevicePreviewsDisabledError extends Schema.TaggedError<DevicePreviewsDisabledError>()(
  "DevicePreviewsDisabledError",
  { message: Schema.String },
) {}

export class DeviceSdkUnavailableError extends Schema.TaggedError<DeviceSdkUnavailableError>()(
  "DeviceSdkUnavailableError",
  { kind: DeviceAvailabilityKind, message: Schema.String },
) {}

/** The platform already runs a device; `running` is that device's display name. */
export class DeviceLimitError extends Schema.TaggedError<DeviceLimitError>()("DeviceLimitError", {
  running: Schema.String,
  message: Schema.String,
}) {}

/** The image already runs outside BiBCode, such as a simulator booted from Simulator. */
export class DeviceBusyError extends Schema.TaggedError<DeviceBusyError>()("DeviceBusyError", {
  imageId: Schema.String,
  message: Schema.String,
}) {}

export class DeviceNotFoundError extends Schema.TaggedError<DeviceNotFoundError>()(
  "DeviceNotFoundError",
  { message: Schema.String },
) {}

export class DeviceNotReadyError extends Schema.TaggedError<DeviceNotReadyError>()(
  "DeviceNotReadyError",
  { state: DeviceState, message: Schema.String },
) {}

export class DeviceInputError extends Schema.TaggedError<DeviceInputError>()("DeviceInputError", {
  message: Schema.String,
}) {}

/** The emulator refused or did not answer a call on a running device. */
export class DeviceEmulatorError extends Schema.TaggedError<DeviceEmulatorError>()(
  "DeviceEmulatorError",
  { message: Schema.String },
) {}

/** The server cannot stream video on this connection (it runs without sign-in). */
export class DeviceMediaUnavailableError extends Schema.TaggedError<DeviceMediaUnavailableError>()(
  "DeviceMediaUnavailableError",
  { message: Schema.String },
) {}

export const DeviceRpcError = Schema.Union([
  DevicePreviewsDisabledError,
  DeviceSdkUnavailableError,
  DeviceLimitError,
  DeviceBusyError,
  DeviceNotFoundError,
  DeviceNotReadyError,
  DeviceInputError,
  DeviceEmulatorError,
  DeviceMediaUnavailableError,
]);
export type DeviceRpcError = typeof DeviceRpcError.Type;
