/**
 * Preview - Schemas for the in-app browser preview surface.
 *
 * The preview is desktop-only: each tab is a Tauri child webview owned by the
 * Rust desktop host (not a renderer-owned Chromium <webview>). The server tracks
 * per-thread tab metadata so it survives client reconnects and multi-window. The
 * desktop renderer mediates: it drives the host's webviews and reports navigation
 * back to the server via these RPCs, the server fans events to all subscribers.
 *
 * @module Preview
 */
import { Schema } from "effect";
import { PortSchema, ThreadId, TrimmedNonEmptyString } from "./baseSchemas.ts";

const Url = TrimmedNonEmptyString.check(Schema.isMaxLength(2048));
const Title = Schema.String.check(Schema.isMaxLength(512));

export const PreviewTabId = TrimmedNonEmptyString.check(Schema.isMaxLength(128));
export type PreviewTabId = typeof PreviewTabId.Type;

export const PREVIEW_VIEWPORT_MIN_DIMENSION = 240;
export const PREVIEW_VIEWPORT_MAX_DIMENSION = 3840;
export const PREVIEW_VIEWPORT_MAX_AREA = 3840 * 2160;

const PreviewViewportDimension = Schema.Int.check(
  Schema.isBetween({
    minimum: PREVIEW_VIEWPORT_MIN_DIMENSION,
    maximum: PREVIEW_VIEWPORT_MAX_DIMENSION,
  }),
);

const viewportAreaFilter = Schema.makeFilter(
  ({ width, height }: { readonly width: number; readonly height: number }) =>
    width * height <= PREVIEW_VIEWPORT_MAX_AREA ||
    `Viewport area must not exceed ${PREVIEW_VIEWPORT_MAX_AREA} pixels.`,
);

export const PreviewViewportSize = Schema.Struct({
  width: PreviewViewportDimension,
  height: PreviewViewportDimension,
}).check(viewportAreaFilter);
export type PreviewViewportSize = typeof PreviewViewportSize.Type;

/**
 * The page's measured viewport can be smaller than the minimum selectable
 * fixed size while fill mode follows a narrow panel. Keep measurement
 * validation separate from the stricter user-selectable size constraints.
 */
export const PreviewRenderedViewportSize = Schema.Struct({
  width: Schema.Int.check(Schema.isGreaterThan(0)),
  height: Schema.Int.check(Schema.isGreaterThan(0)),
});
export type PreviewRenderedViewportSize = typeof PreviewRenderedViewportSize.Type;

export const PREVIEW_VIEWPORT_PRESET_IDS = [
  "iphone-se",
  "iphone-xr",
  "iphone-12-pro",
  "iphone-14-pro-max",
  "pixel-7",
  "samsung-galaxy-s8-plus",
  "samsung-galaxy-s20-ultra",
  "ipad-mini",
  "ipad-air",
  "ipad-pro",
  "surface-pro-7",
  "surface-duo",
  "galaxy-z-fold-5",
  "asus-zenbook-fold",
  "samsung-galaxy-a51-71",
  "nest-hub",
  "nest-hub-max",
] as const;

export const PreviewViewportPresetId = Schema.Literals(PREVIEW_VIEWPORT_PRESET_IDS);
export type PreviewViewportPresetId = typeof PreviewViewportPresetId.Type;

/**
 * Preset IDs shipped before the Chrome-compatible catalog. Existing sessions
 * can still reconnect with these values, but new resize requests only expose
 * PREVIEW_VIEWPORT_PRESET_IDS.
 */
const LEGACY_PREVIEW_VIEWPORT_PRESET_IDS = [
  "desktop-1920x1080",
  "desktop-1440x900",
  "laptop-1366x768",
  "laptop-1280x800",
  "ipad-pro-11",
  "iphone-15-pro",
  "pixel-8",
  "galaxy-s24",
] as const;

const StoredPreviewViewportPresetId = Schema.Literals([
  ...PREVIEW_VIEWPORT_PRESET_IDS,
  ...LEGACY_PREVIEW_VIEWPORT_PRESET_IDS,
]);

export const PreviewViewportSetting = Schema.Union([
  Schema.TaggedStruct("fill", {}),
  Schema.TaggedStruct("freeform", {
    ...PreviewViewportSize.fields,
  }).check(viewportAreaFilter),
  Schema.TaggedStruct("preset", {
    ...PreviewViewportSize.fields,
    presetId: StoredPreviewViewportPresetId,
  }).check(viewportAreaFilter),
]);
export type PreviewViewportSetting = typeof PreviewViewportSetting.Type;

export const FILL_PREVIEW_VIEWPORT = {
  _tag: "fill",
} as const satisfies PreviewViewportSetting;

export const PreviewNavStatus = Schema.Union([
  Schema.TaggedStruct("Idle", {}),
  Schema.TaggedStruct("Loading", {
    url: Url,
    title: Title,
  }),
  Schema.TaggedStruct("Success", {
    url: Url,
    title: Title,
  }),
  Schema.TaggedStruct("LoadFailed", {
    url: Url,
    title: Title,
    code: Schema.Int,
    description: Schema.String,
  }),
]);
export type PreviewNavStatus = typeof PreviewNavStatus.Type;

export const PreviewSessionSnapshot = Schema.Struct({
  threadId: TrimmedNonEmptyString,
  tabId: PreviewTabId,
  navStatus: PreviewNavStatus,
  canGoBack: Schema.Boolean,
  canGoForward: Schema.Boolean,
  /** Missing snapshots from older servers are treated as fill-panel mode. */
  viewport: Schema.optional(PreviewViewportSetting),
  updatedAt: Schema.String,
});
export type PreviewSessionSnapshot = typeof PreviewSessionSnapshot.Type;

export const PreviewOpenInput = Schema.Struct({
  threadId: ThreadId,
  /** Omit to create an empty (Idle) tab the user can type into. */
  url: Schema.optional(Url),
});
export type PreviewOpenInput = typeof PreviewOpenInput.Type;

export const PreviewNavigateInput = Schema.Struct({
  threadId: ThreadId,
  tabId: PreviewTabId,
  url: Url,
  resolvedTitle: Schema.optional(Title),
});
export type PreviewNavigateInput = typeof PreviewNavigateInput.Type;

export const PreviewReportStatusInput = Schema.Struct({
  threadId: ThreadId,
  tabId: PreviewTabId,
  navStatus: PreviewNavStatus,
  canGoBack: Schema.Boolean,
  canGoForward: Schema.Boolean,
});
export type PreviewReportStatusInput = typeof PreviewReportStatusInput.Type;

export const PreviewRefreshInput = Schema.Struct({
  threadId: ThreadId,
  tabId: PreviewTabId,
});
export type PreviewRefreshInput = typeof PreviewRefreshInput.Type;

export const PreviewResizeInput = Schema.Struct({
  threadId: ThreadId,
  tabId: PreviewTabId,
  viewport: PreviewViewportSetting,
});
export type PreviewResizeInput = typeof PreviewResizeInput.Type;

export const PreviewCloseInput = Schema.Struct({
  threadId: ThreadId,
  tabId: Schema.optional(PreviewTabId),
});
export type PreviewCloseInput = typeof PreviewCloseInput.Type;

export const PreviewListInput = Schema.Struct({
  threadId: ThreadId,
});
export type PreviewListInput = typeof PreviewListInput.Type;

export const PreviewClaimOpenRequestInput = Schema.Struct({
  requestId: TrimmedNonEmptyString,
});
export type PreviewClaimOpenRequestInput = typeof PreviewClaimOpenRequestInput.Type;

/** `claimed` is false when another client claimed the request first or it expired. */
export const PreviewClaimOpenRequestResult = Schema.Struct({
  claimed: Schema.Boolean,
});
export type PreviewClaimOpenRequestResult = typeof PreviewClaimOpenRequestResult.Type;

/** `url` is the canonical loopback URL; the server admits only plain-HTTP loopback targets. */
export const PreviewGatewayOpenInput = Schema.Struct({
  threadId: ThreadId,
  url: Url,
});
export type PreviewGatewayOpenInput = typeof PreviewGatewayOpenInput.Type;

/**
 * `capability` bootstraps one gateway session on `gatewayPort` before `expiresAtMs`.
 * `origin` is set for a caller that came through BiBCode Connect: the preview
 * hostname's `https://` origin to bootstrap on instead of the server's address.
 */
export const PreviewGatewayOpenResult = Schema.Struct({
  gatewayPort: PortSchema,
  capability: TrimmedNonEmptyString,
  expiresAtMs: Schema.Finite,
  origin: Schema.optional(TrimmedNonEmptyString),
});
export type PreviewGatewayOpenResult = typeof PreviewGatewayOpenResult.Type;

/** Opens a preview tunnel to `port` on the server's loopback (#69). */
export const PreviewTunnelOpenInput = Schema.Struct({
  port: PortSchema,
});
export type PreviewTunnelOpenInput = typeof PreviewTunnelOpenInput.Type;

/**
 * A preview tunnel's stream: `Opened` first, with the id `preview.tunnelWrite` and
 * `preview.tunnelClose` name, then `Data` (base64) as the upstream sends. The stream ends
 * when the upstream closes.
 */
export const PreviewTunnelEvent = Schema.Union([
  Schema.TaggedStruct("Opened", { tunnelId: TrimmedNonEmptyString }),
  Schema.TaggedStruct("Data", { data: Schema.String }),
]);
export type PreviewTunnelEvent = typeof PreviewTunnelEvent.Type;

/** One base64 chunk for the upstream; the call returns once it is written. */
export const PreviewTunnelWriteInput = Schema.Struct({
  tunnelId: TrimmedNonEmptyString,
  data: Schema.String,
});
export type PreviewTunnelWriteInput = typeof PreviewTunnelWriteInput.Type;

export const PreviewTunnelCloseInput = Schema.Struct({
  tunnelId: TrimmedNonEmptyString,
});
export type PreviewTunnelCloseInput = typeof PreviewTunnelCloseInput.Type;

export const PreviewListResult = Schema.Struct({
  sessions: Schema.Array(PreviewSessionSnapshot),
});
export type PreviewListResult = typeof PreviewListResult.Type;

const PreviewEventBaseSchema = Schema.Struct({
  threadId: TrimmedNonEmptyString,
  tabId: PreviewTabId,
  createdAt: Schema.String,
});

const PreviewOpenedEvent = Schema.Struct({
  ...PreviewEventBaseSchema.fields,
  type: Schema.Literal("opened"),
  snapshot: PreviewSessionSnapshot,
});

const PreviewNavigatedEvent = Schema.Struct({
  ...PreviewEventBaseSchema.fields,
  type: Schema.Literal("navigated"),
  snapshot: PreviewSessionSnapshot,
});

const PreviewResizedEvent = Schema.Struct({
  ...PreviewEventBaseSchema.fields,
  type: Schema.Literal("resized"),
  snapshot: PreviewSessionSnapshot,
});

const PreviewFailedEvent = Schema.Struct({
  ...PreviewEventBaseSchema.fields,
  type: Schema.Literal("failed"),
  url: Url,
  title: Title,
  code: Schema.Int,
  description: Schema.String,
});

const PreviewClosedEvent = Schema.Struct({
  ...PreviewEventBaseSchema.fields,
  type: Schema.Literal("closed"),
});

/**
 * A command in the thread (an agent's `$BROWSER`, a CLI) asked to open `url`.
 * It names no tab: the first client to claim `requestId` opens it.
 */
export const PreviewOpenRequestedEvent = Schema.Struct({
  type: Schema.Literal("openRequested"),
  threadId: TrimmedNonEmptyString,
  requestId: TrimmedNonEmptyString,
  url: Url,
  createdAt: Schema.String,
});
export type PreviewOpenRequestedEvent = typeof PreviewOpenRequestedEvent.Type;

export const PreviewEvent = Schema.Union([
  PreviewOpenedEvent,
  PreviewNavigatedEvent,
  PreviewResizedEvent,
  PreviewFailedEvent,
  PreviewClosedEvent,
  PreviewOpenRequestedEvent,
]);
export type PreviewEvent = typeof PreviewEvent.Type;

/**
 * A localhost server detected by the port scanner. Used to populate the
 * "Local" recommendations in the empty-state of the preview panel.
 */
export const DiscoveredLocalServer = Schema.Struct({
  host: TrimmedNonEmptyString,
  port: Schema.Int.check(Schema.isGreaterThan(0)).check(Schema.isLessThan(65536)),
  url: Url,
  processName: Schema.NullOr(TrimmedNonEmptyString),
  pid: Schema.NullOr(Schema.Int.check(Schema.isGreaterThan(0))),
  terminal: Schema.NullOr(
    Schema.Struct({
      threadId: ThreadId,
      terminalId: TrimmedNonEmptyString,
    }),
  ),
});
export type DiscoveredLocalServer = typeof DiscoveredLocalServer.Type;

export const DiscoveredLocalServerList = Schema.Struct({
  servers: Schema.Array(DiscoveredLocalServer),
  scannedAt: Schema.String,
});
export type DiscoveredLocalServerList = typeof DiscoveredLocalServerList.Type;

export class PreviewSessionLookupError extends Schema.TaggedError<PreviewSessionLookupError>()(
  "PreviewSessionLookupError",
  {
    threadId: Schema.String,
    tabId: Schema.String,
  },
) {
  override get message() {
    return `Unknown preview session: thread=${this.threadId}, tab=${this.tabId}`;
  }
}

export class PreviewInvalidUrlError extends Schema.TaggedError<PreviewInvalidUrlError>()(
  "PreviewInvalidUrlError",
  {
    inputLength: Schema.Finite,
    reason: Schema.Literals(["empty", "parse", "unsupported-protocol", "unexpected"]),
    protocol: Schema.optional(Schema.String),
    cause: Schema.Defect(),
  },
) {
  override get message() {
    const protocol = this.protocol === undefined ? "" : `: ${this.protocol}`;
    return `Invalid preview URL (${this.reason}${protocol}; input length ${this.inputLength}).`;
  }
}

/**
 * Why `preview.gatewayOpen` refused: `not-admitted` (not a plain-HTTP loopback URL),
 * `https-unsupported`, `no-upstream` (nothing listens on the port), `not-reachable` (the
 * caller reached the server on a public address or through a reverse proxy),
 * `relay-not-provisioned` (a BiBCode Connect caller whose environment has no preview
 * hostnames yet), `relay-busy` (every preview hostname serves a target with live
 * connections), or `unavailable`.
 */
export class PreviewGatewayError extends Schema.TaggedError<PreviewGatewayError>()(
  "PreviewGatewayError",
  {
    reason: Schema.Literals([
      "not-admitted",
      "https-unsupported",
      "no-upstream",
      "not-reachable",
      "relay-not-provisioned",
      "relay-busy",
      "unavailable",
    ]),
    message: Schema.String,
  },
) {}

/**
 * Why a preview tunnel was refused or ended: `not-admitted` (port 0, or the server's own
 * port), `no-upstream` (nothing listens on the port), `busy` (too many open tunnels),
 * `closed` (the tunnel is gone), `invalid-data` (not base64, or too large),
 * `revoked` (the session that opened it ended), or `unavailable`.
 */
export class PreviewTunnelError extends Schema.TaggedError<PreviewTunnelError>()(
  "PreviewTunnelError",
  {
    reason: Schema.Literals([
      "not-admitted",
      "no-upstream",
      "busy",
      "closed",
      "invalid-data",
      "revoked",
      "unavailable",
    ]),
    message: Schema.String,
  },
) {}

export const PreviewError = Schema.Union([PreviewSessionLookupError, PreviewInvalidUrlError]);
export type PreviewError = typeof PreviewError.Type;
