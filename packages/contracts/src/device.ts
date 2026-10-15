import * as Schema from "effect/Schema";

export const DeviceAvailabilityKind = Schema.Literals([
  "ready",
  "sdkMissing",
  "adbMissing",
  "noAvds",
  "accelerationUnavailable",
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

/** `width` and `height` are the display size in the current orientation. */
export const DeviceSnapshot = Schema.Struct({
  deviceId: Schema.String,
  avdName: Schema.String,
  state: DeviceState,
  failure: Schema.optionalKey(Schema.String),
  width: Schema.optionalKey(Schema.Number),
  height: Schema.optionalKey(Schema.Number),
  orientation: DeviceOrientation,
});
export type DeviceSnapshot = typeof DeviceSnapshot.Type;

export const DeviceCatalog = Schema.Struct({
  availability: Schema.Struct({ kind: DeviceAvailabilityKind, message: Schema.String }),
  avds: Schema.Array(Schema.Struct({ name: Schema.String, displayName: Schema.String })),
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

export const DeviceOpenInput = Schema.Struct({ avdName: Schema.String });
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

export class DeviceLimitError extends Schema.TaggedError<DeviceLimitError>()("DeviceLimitError", {
  running: Schema.String,
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

export const DeviceRpcError = Schema.Union([
  DevicePreviewsDisabledError,
  DeviceSdkUnavailableError,
  DeviceLimitError,
  DeviceNotFoundError,
  DeviceNotReadyError,
  DeviceInputError,
  DeviceEmulatorError,
]);
export type DeviceRpcError = typeof DeviceRpcError.Type;
