import type {
  VcsCreateRefInput,
  VcsCreateRefResult,
  VcsInitInput,
  VcsListRefsInput,
  VcsListRefsResult,
  VcsPullInput,
  VcsPullResult,
  VcsSwitchRefInput,
  VcsSwitchRefResult,
  GitPreparePullRequestThreadInput,
  GitPreparePullRequestThreadResult,
  GitPullRequestRefInput,
  GitResolvePullRequestResult,
  VcsStatusInput,
  VcsStatusResult,
} from "./git.ts";
import type { ReviewDiffPreviewInput, ReviewDiffPreviewResult } from "./review.ts";
import type { FilesystemBrowseInput, FilesystemBrowseResult } from "./filesystem.ts";
import type { AssetCreateUrlInput, AssetCreateUrlResult } from "./assets.ts";
import type {
  ProjectListEntriesInput,
  ProjectListEntriesResult,
  ProjectReadFileInput,
  ProjectReadFileResult,
  ProjectSearchEntriesInput,
  ProjectSearchEntriesResult,
  ProjectWriteFileInput,
  ProjectWriteFileResult,
} from "./project.ts";
import type { ProviderInstanceId } from "./providerInstance.ts";
import type {
  ServerConfig,
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryInput,
  ServerProcessResourceHistoryResult,
  ServerProviderUpdateInput,
  ServerProviderUpdatedPayload,
  ServerRemoveKeybindingResult,
  ServerSignalProcessInput,
  ServerSignalProcessResult,
  ServerTraceDiagnosticsResult,
  ServerUpsertKeybindingResult,
} from "./server.ts";
import type {
  TerminalAttachInput,
  TerminalAttachStreamEvent,
  TerminalClearInput,
  TerminalCloseInput,
  TerminalMetadataStreamEvent,
  TerminalOpenInput,
  TerminalResizeInput,
  TerminalRestartInput,
  TerminalSessionSnapshot,
  TerminalWriteInput,
} from "./terminal.ts";
import type { ServerRemoveKeybindingInput, ServerUpsertKeybindingInput } from "./server.ts";
import * as Effect from "effect/Effect";
import * as Schema from "effect/Schema";
import type {
  DiscoveredLocalServerList,
  PreviewCloseInput,
  PreviewEvent,
  PreviewListInput,
  PreviewListResult,
  PreviewNavigateInput,
  PreviewOpenInput,
  PreviewRefreshInput,
  PreviewReportStatusInput,
  PreviewResizeInput,
  PreviewSessionSnapshot,
} from "./preview.ts";
import {
  PreviewAutomationClickInput,
  PreviewAutomationEvaluateInput,
  PreviewAutomationHost,
  PreviewAutomationHostFocus,
  PreviewAutomationPressInput,
  PreviewAutomationResponse,
  PreviewAutomationScrollInput,
  PreviewAutomationSnapshot,
  PreviewAutomationStatus,
  PreviewAutomationStreamEvent,
  PreviewAutomationTypeInput,
  PreviewAutomationWaitForInput,
} from "./previewAutomation.ts";
import type {
  ClientOrchestrationCommand,
  DispatchResult,
  OrchestrationGetFullThreadDiffInput,
  OrchestrationGetFullThreadDiffResult,
  OrchestrationGetTurnDiffInput,
  OrchestrationGetTurnDiffResult,
  OrchestrationShellSnapshot,
  OrchestrationShellStreamItem,
  OrchestrationSubscribeThreadInput,
  OrchestrationThreadStreamItem,
} from "./orchestration.ts";
import { EnvironmentId, NonNegativeInt, PortSchema } from "./baseSchemas.ts";
import {
  AuthAccessTokenResult,
  type AuthEnvironmentScope,
  AuthSessionState,
  AuthWebSocketTicketResult,
} from "./auth.ts";
import { AdvertisedEndpoint } from "./remoteAccess.ts";
import { EditorId } from "./editor.ts";
import { ExecutionEnvironmentDescriptor } from "./environment.ts";
import type { ClientSettings, ServerSettings, ServerSettingsPatch } from "./settings.ts";
import type { ProfilerClientBatch, ProfilerClientBatchResult, ProfilerStatus } from "./profiler.ts";
import type {
  SourceControlCloneRepositoryInput,
  SourceControlCloneRepositoryResult,
  SourceControlDiscoveryResult,
  SourceControlPublishRepositoryInput,
  SourceControlPublishRepositoryResult,
  SourceControlRepositoryInfo,
  SourceControlRepositoryLookupInput,
} from "./sourceControl.ts";

/**
 * A divider between groups of a context menu. Renderers drop leading, trailing
 * and repeated separators, so callers can build groups without tracking which
 * neighbouring items were omitted.
 */
export interface ContextMenuSeparator {
  readonly separator: true;
}

export interface ContextMenuItem<T extends string = string> {
  id: T;
  label: string;
  destructive?: boolean;
  disabled?: boolean;
  /** Explains an unavailable action; exposed by renderers to sighted and assistive users. */
  description?: string;
  /** Renders as a non-interactive section header label. Web fallback only — stripped on desktop native menus. */
  header?: boolean;
  /** Icon keyword resolved by the web fallback. Stripped on desktop native menus. */
  icon?: string;
  children?: readonly ContextMenuEntry<T>[];
}

/** One row of a context menu: an actionable item or a separator. */
export type ContextMenuEntry<T extends string = string> = ContextMenuItem<T> | ContextMenuSeparator;

export interface ContextMenuSeparatorSchemaType {
  readonly separator: true;
}

export interface ContextMenuItemSchemaType {
  readonly id: string;
  readonly label: string;
  readonly destructive?: boolean;
  readonly disabled?: boolean;
  readonly description?: string;
  readonly header?: boolean;
  readonly icon?: string;
  readonly children?: readonly ContextMenuEntrySchemaType[];
}

export type ContextMenuEntrySchemaType = ContextMenuItemSchemaType | ContextMenuSeparatorSchemaType;

export const ContextMenuSeparatorSchema: Schema.Codec<ContextMenuSeparatorSchemaType> =
  Schema.Struct({
    separator: Schema.Literal(true),
  });

export const ContextMenuItemSchema: Schema.Codec<ContextMenuItemSchemaType> = Schema.Struct({
  id: Schema.String,
  label: Schema.String,
  destructive: Schema.optionalKey(Schema.Boolean),
  disabled: Schema.optionalKey(Schema.Boolean),
  description: Schema.optionalKey(Schema.String),
  header: Schema.optionalKey(Schema.Boolean),
  icon: Schema.optionalKey(Schema.String),
  children: Schema.optionalKey(
    Schema.Array(
      Schema.suspend((): Schema.Codec<ContextMenuEntrySchemaType> => ContextMenuEntrySchema),
    ),
  ),
});

export const ContextMenuEntrySchema: Schema.Codec<ContextMenuEntrySchemaType> = Schema.Union([
  ContextMenuItemSchema,
  ContextMenuSeparatorSchema,
]);

export type DesktopUpdateStatus =
  | "disabled"
  | "idle"
  | "checking"
  | "up-to-date"
  | "available"
  | "downloading"
  | "downloaded"
  | "error";

export type DesktopUpdatePhase =
  | "idle"
  | "checking"
  | "available"
  | "protecting"
  | "installing"
  | "failed";

export type DesktopUpdateProtectionStatus =
  | "pending"
  | "protected"
  | "failed"
  | "excluded"
  | "skipped";

export type DesktopUpdateProtectionStage =
  | "waiting-for-mutations"
  | "quiescing-runtime"
  | "acquiring-store-lock"
  | "checkpointing-database"
  | "creating-verified-backup"
  | "stopping-backend";

export interface DesktopUpdateProtection {
  environmentId: string;
  label: string;
  status: DesktopUpdateProtectionStatus;
  message: string | null;
  stage?: DesktopUpdateProtectionStage | null;
  elapsedMs?: number | null;
  blockedOperationCount?: number | null;
}

export interface DesktopUpdateInstallInput {
  excludedEnvironmentIds?: readonly string[];
  skipProtection?: boolean;
}

export interface DesktopBackendRecovery {
  environmentId: string;
  label: string;
  reason: "port-in-use" | "other";
  port: number;
}

export const DesktopBackendRecoverySchema = Schema.Struct({
  environmentId: Schema.String,
  label: Schema.String,
  reason: Schema.Literals(["port-in-use", "other"]),
  port: PortSchema,
});

export type DesktopRuntimeArch = "arm64" | "x64" | "other";
export type DesktopTheme = "light" | "dark" | "system";
export type DesktopAppStageLabel = "Dev" | "Latest" | "Nightly";
export type DesktopBridgeHost = "tauri";

export const DesktopUpdateStatusSchema = Schema.Literals([
  "disabled",
  "idle",
  "checking",
  "up-to-date",
  "available",
  "downloading",
  "downloaded",
  "error",
]);
export const DesktopUpdatePhaseSchema = Schema.Literals([
  "idle",
  "checking",
  "available",
  "protecting",
  "installing",
  "failed",
]);
export const DesktopUpdateProtectionStageSchema = Schema.Literals([
  "waiting-for-mutations",
  "quiescing-runtime",
  "acquiring-store-lock",
  "checkpointing-database",
  "creating-verified-backup",
  "stopping-backend",
]);
export const DesktopUpdateProtectionSchema = Schema.Struct({
  environmentId: Schema.String,
  label: Schema.String,
  status: Schema.Literals(["pending", "protected", "failed", "excluded", "skipped"]),
  message: Schema.NullOr(Schema.String),
  stage: Schema.NullOr(DesktopUpdateProtectionStageSchema).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
  elapsedMs: Schema.NullOr(NonNegativeInt).pipe(Schema.withDecodingDefault(Effect.succeed(null))),
  blockedOperationCount: Schema.NullOr(NonNegativeInt).pipe(
    Schema.withDecodingDefault(Effect.succeed(null)),
  ),
});

export type DesktopProjectDataStatus =
  | "healthy"
  | "storage-changed"
  | "recovery-required"
  | "unavailable";

export interface DesktopProjectDataStatusChangedEvent {
  readonly environmentId: string;
}

export interface DesktopProjectDataBackup {
  backupId: string;
  createdAt: string;
  trigger: "pre-migration" | "pre-update";
  appVersion: string;
  schemaVersion: number;
  sizeBytes: number;
}

export const DesktopProjectDataBackupSchema = Schema.Struct({
  backupId: Schema.String,
  createdAt: Schema.String,
  trigger: Schema.Literals(["pre-migration", "pre-update"]),
  appVersion: Schema.String,
  schemaVersion: Schema.Finite,
  sizeBytes: Schema.Finite,
});

export interface DesktopProjectDataEnvironmentStatus {
  environmentId: string;
  label: string;
  runningDistro: string | null;
  status: DesktopProjectDataStatus;
  requestedRoot: string;
  effectiveRoot: string;
  isFilesystemAlias: boolean;
  storageInstanceId: string | null;
  issue: string | null;
  backups: ReadonlyArray<DesktopProjectDataBackup>;
}

export const DesktopProjectDataEnvironmentStatusSchema = Schema.Struct({
  environmentId: Schema.String,
  label: Schema.String,
  runningDistro: Schema.NullOr(Schema.String),
  status: Schema.Literals(["healthy", "storage-changed", "recovery-required", "unavailable"]),
  requestedRoot: Schema.String,
  effectiveRoot: Schema.String,
  isFilesystemAlias: Schema.Boolean,
  storageInstanceId: Schema.NullOr(Schema.String),
  issue: Schema.NullOr(Schema.String),
  backups: Schema.Array(DesktopProjectDataBackupSchema),
});

export interface DesktopProjectDataRecoveryResult {
  environmentId: string;
  action: "restore" | "start-empty";
  committed: boolean;
  preservedDirectory: string;
  storageInstanceId: string | null;
  restartError: string | null;
}

export const DesktopProjectDataRecoveryResultSchema = Schema.Struct({
  environmentId: Schema.String,
  action: Schema.Literals(["restore", "start-empty"]),
  committed: Schema.Boolean,
  preservedDirectory: Schema.String,
  storageInstanceId: Schema.NullOr(Schema.String),
  restartError: Schema.NullOr(Schema.String),
});
export const DesktopRuntimeArchSchema = Schema.Literals(["arm64", "x64", "other"]);
export const DesktopThemeSchema = Schema.Literals(["light", "dark", "system"]);
export const DesktopAppStageLabelSchema = Schema.Literals(["Dev", "Latest", "Nightly"]);
export const DesktopBridgeHostSchema = Schema.Literal("tauri");

export interface DesktopBridgeFeatureFlags {
  localBackend: boolean;
  localBearerToken: boolean;
  clientSettings: boolean;
  serverExposure: boolean;
  wslDiscovery: boolean;
  sshRemoteHttp: boolean;
  connectionCatalog: boolean;
  protectedConnectionCatalog: boolean;
  sshProvisioning: boolean;
  preview: boolean;
  updater: boolean;
  menuEvents: boolean;
}

export const DesktopBridgeFeatureFlagsSchema = Schema.Struct({
  localBackend: Schema.Boolean,
  localBearerToken: Schema.Boolean,
  clientSettings: Schema.Boolean,
  serverExposure: Schema.Boolean,
  wslDiscovery: Schema.Boolean,
  sshRemoteHttp: Schema.Boolean,
  connectionCatalog: Schema.Boolean,
  protectedConnectionCatalog: Schema.Boolean,
  sshProvisioning: Schema.Boolean,
  preview: Schema.Boolean,
  updater: Schema.Boolean,
  menuEvents: Schema.Boolean,
});

export interface DesktopBridgeHostMetadata {
  host: DesktopBridgeHost;
  bridgeVersion: number;
  features: DesktopBridgeFeatureFlags;
}

export const DesktopBridgeHostMetadataSchema = Schema.Struct({
  host: DesktopBridgeHostSchema,
  bridgeVersion: Schema.Finite,
  features: DesktopBridgeFeatureFlagsSchema,
});

export interface DesktopAppBranding {
  baseName: string;
  stageLabel: DesktopAppStageLabel;
  displayName: string;
}

export const DesktopAppBrandingSchema = Schema.Struct({
  baseName: Schema.String,
  stageLabel: DesktopAppStageLabelSchema,
  displayName: Schema.String,
});

export interface DesktopRuntimeInfo {
  hostArch: DesktopRuntimeArch;
  appArch: DesktopRuntimeArch;
  runningUnderArm64Translation: boolean;
}

export const DesktopRuntimeInfoSchema = Schema.Struct({
  hostArch: DesktopRuntimeArchSchema,
  appArch: DesktopRuntimeArchSchema,
  runningUnderArm64Translation: Schema.Boolean,
});

export interface DesktopUpdateRequester {
  readonly label: string;
  readonly detail: string | null;
}

export interface DesktopUpdateState {
  enabled: boolean;
  status: DesktopUpdateStatus;
  currentVersion: string;
  hostArch: DesktopRuntimeArch;
  appArch: DesktopRuntimeArch;
  runningUnderArm64Translation: boolean;
  availableVersion: string | null;
  downloadedVersion: string | null;
  downloadPercent: number | null;
  checkedAt: string | null;
  message: string | null;
  errorContext: "check" | "download" | "install" | null;
  canRetry: boolean;
  phase?: DesktopUpdatePhase;
  protection?: ReadonlyArray<DesktopUpdateProtection>;
  backendRecovery?: ReadonlyArray<DesktopBackendRecovery>;
  requestedBy?: DesktopUpdateRequester | null;
}

export const DesktopUpdateStateSchema = Schema.Struct({
  enabled: Schema.Boolean,
  status: DesktopUpdateStatusSchema,
  currentVersion: Schema.String,
  hostArch: DesktopRuntimeArchSchema,
  appArch: DesktopRuntimeArchSchema,
  runningUnderArm64Translation: Schema.Boolean,
  availableVersion: Schema.NullOr(Schema.String),
  downloadedVersion: Schema.NullOr(Schema.String),
  downloadPercent: Schema.NullOr(Schema.Finite),
  checkedAt: Schema.NullOr(Schema.String),
  message: Schema.NullOr(Schema.String),
  errorContext: Schema.NullOr(Schema.Literals(["check", "download", "install"])),
  canRetry: Schema.Boolean,
  phase: DesktopUpdatePhaseSchema.pipe(Schema.withDecodingDefault(Effect.succeed("idle"))),
  protection: Schema.Array(DesktopUpdateProtectionSchema).pipe(
    Schema.withDecodingDefault(Effect.succeed([])),
  ),
  backendRecovery: Schema.Array(DesktopBackendRecoverySchema).pipe(
    Schema.withDecodingDefault(Effect.succeed([])),
  ),
  requestedBy: Schema.NullOr(
    Schema.Struct({ label: Schema.String, detail: Schema.NullOr(Schema.String) }),
  ).pipe(Schema.withDecodingDefault(Effect.succeed(null))),
});

export interface DesktopUpdateActionResult {
  accepted: boolean;
  completed: boolean;
  state: DesktopUpdateState;
}

export const DesktopUpdateActionResultSchema = Schema.Struct({
  accepted: Schema.Boolean,
  completed: Schema.Boolean,
  state: DesktopUpdateStateSchema,
});

export interface DesktopUpdateCheckResult {
  checked: boolean;
  state: DesktopUpdateState;
}

export const DesktopUpdateCheckResultSchema = Schema.Struct({
  checked: Schema.Boolean,
  state: DesktopUpdateStateSchema,
});

// Stable id for the Windows-native primary backend. Desktop side wraps
// this with a brand inside DesktopBackendManager; web side keeps it as
// a plain string so the env-runtime can compare against it without
// importing brand machinery from the desktop package.
export const PRIMARY_LOCAL_ENVIRONMENT_ID = "primary";

export interface DesktopWslPrimaryUnavailableError {
  kind: "wsl-primary-unavailable";
  detail: string;
}

export const DesktopWslPrimaryUnavailableErrorSchema = Schema.Struct({
  kind: Schema.Literal("wsl-primary-unavailable"),
  detail: Schema.String,
});

export interface DesktopWslSecondaryUnavailableError {
  kind: "wsl-secondary-unavailable";
  detail: string;
}

export const DesktopWslSecondaryUnavailableErrorSchema = Schema.Struct({
  kind: Schema.Literal("wsl-secondary-unavailable"),
  detail: Schema.String,
});

export type DesktopWslPreflightError =
  | DesktopWslPrimaryUnavailableError
  | DesktopWslSecondaryUnavailableError;

export const DesktopWslPreflightErrorSchema = Schema.Union([
  DesktopWslPrimaryUnavailableErrorSchema,
  DesktopWslSecondaryUnavailableErrorSchema,
]);

export interface DesktopEnvironmentBootstrap {
  // Stable backend instance id (e.g. "primary" or "wsl:ubuntu"). The
  // web env runtime keys local environments off this so projects
  // routed to a specific backend reopen against the same one.
  id: string;
  label: string;
  // Concrete WSL distro used by the current backend run. This stays separate
  // from id because a default-tracking instance keeps the stable
  // "wsl:default" IPC target while each run launches a specific distro.
  runningDistro?: string | null;
  // The explicit distro whose desired registration this entry represents.
  // null/absent means the stable default-tracking WSL registration.
  configuredDistro?: string | null;
  httpBaseUrl: string | null;
  wsBaseUrl: string | null;
  bootstrapToken?: string;
  // Present only when this desired WSL secondary could not be planned or
  // started. Unavailable entries intentionally have no endpoint or token.
  preflightError?: DesktopWslSecondaryUnavailableError | null;
}

export const DesktopEnvironmentBootstrapSchema = Schema.Struct({
  id: Schema.String,
  label: Schema.String,
  runningDistro: Schema.optionalKey(Schema.NullOr(Schema.String)),
  configuredDistro: Schema.optionalKey(Schema.NullOr(Schema.String)),
  httpBaseUrl: Schema.NullOr(Schema.String),
  wsBaseUrl: Schema.NullOr(Schema.String),
  bootstrapToken: Schema.optionalKey(Schema.String),
  preflightError: Schema.optionalKey(Schema.NullOr(DesktopWslSecondaryUnavailableErrorSchema)),
});

export const DesktopSshEnvironmentTargetSchema = Schema.Struct({
  alias: Schema.String,
  hostname: Schema.String,
  username: Schema.NullOr(Schema.String),
  port: Schema.NullOr(Schema.Finite),
});
export type DesktopSshEnvironmentTarget = typeof DesktopSshEnvironmentTargetSchema.Type;

export type DesktopSshHostSource = "ssh-config" | "known-hosts";
export const DesktopSshHostSourceSchema = Schema.Literals(["ssh-config", "known-hosts"]);

export interface DesktopDiscoveredSshHost extends DesktopSshEnvironmentTarget {
  source: DesktopSshHostSource;
}

export const DesktopDiscoveredSshHostSchema = Schema.Struct({
  alias: Schema.String,
  hostname: Schema.String,
  username: Schema.NullOr(Schema.String),
  port: Schema.NullOr(Schema.Finite),
  source: DesktopSshHostSourceSchema,
});

export interface DesktopSshEnvironmentBootstrap {
  target: DesktopSshEnvironmentTarget;
  httpBaseUrl: string;
  wsBaseUrl: string;
  pairingToken: string | null;
  remotePort?: number;
  remoteServerKind?: "external" | "managed";
}

export const DesktopSshEnvironmentBootstrapSchema = Schema.Struct({
  target: DesktopSshEnvironmentTargetSchema,
  httpBaseUrl: Schema.String,
  wsBaseUrl: Schema.String,
  pairingToken: Schema.NullOr(Schema.String),
  remotePort: Schema.optionalKey(Schema.Finite),
  remoteServerKind: Schema.optionalKey(Schema.Literals(["external", "managed"])),
});

export interface DesktopSshPasswordPromptRequest {
  requestId: string;
  destination: string;
  username: string | null;
  prompt: string;
  expiresAt: string;
}

export const DesktopSshPasswordPromptRequestSchema = Schema.Struct({
  requestId: Schema.String,
  destination: Schema.String,
  username: Schema.NullOr(Schema.String),
  prompt: Schema.String,
  expiresAt: Schema.String,
});

export const DesktopSshPasswordPromptCancelledType = "ssh-password-prompt-cancelled" as const;

export const DesktopSshPasswordPromptCancelledResultSchema = Schema.Struct({
  type: Schema.Literal(DesktopSshPasswordPromptCancelledType),
  message: Schema.String,
});

export const DesktopSshEnvironmentEnsureOptionsSchema = Schema.Struct({
  issuePairingToken: Schema.optionalKey(Schema.Boolean),
});

export const DesktopSshEnvironmentEnsureInputSchema = Schema.Struct({
  target: DesktopSshEnvironmentTargetSchema,
  options: Schema.optionalKey(DesktopSshEnvironmentEnsureOptionsSchema),
});

export const DesktopSshEnvironmentEnsureResultSchema = Schema.Union([
  DesktopSshEnvironmentBootstrapSchema,
  DesktopSshPasswordPromptCancelledResultSchema,
]);

export const DesktopSshHttpBaseUrlInputSchema = Schema.Struct({
  httpBaseUrl: Schema.String,
});

export const DesktopSshBearerRequestInputSchema = Schema.Struct({
  httpBaseUrl: Schema.String,
  bearerToken: Schema.String,
});

export const DesktopSshBearerBootstrapInputSchema = Schema.Struct({
  httpBaseUrl: Schema.String,
  credential: Schema.String,
});

export const DesktopSshPasswordPromptResolutionInputSchema = Schema.Struct({
  requestId: Schema.String,
  password: Schema.NullOr(Schema.String),
});

export const PersistedSavedEnvironmentRecordSchema = Schema.Struct({
  environmentId: EnvironmentId,
  label: Schema.String,
  wsBaseUrl: Schema.String,
  httpBaseUrl: Schema.String,
  createdAt: Schema.String,
  lastConnectedAt: Schema.NullOr(Schema.String),
  desktopSsh: Schema.optionalKey(DesktopSshEnvironmentTargetSchema),
  relayManaged: Schema.optionalKey(
    Schema.Struct({
      relayUrl: Schema.String,
    }),
  ),
});
export type PersistedSavedEnvironmentRecord = typeof PersistedSavedEnvironmentRecordSchema.Type;

export type DesktopServerExposureMode = "local-only" | "network-accessible";
export type DesktopServerExposureManagement = "native" | "external";

export const DesktopServerExposureModeSchema = Schema.Literals([
  "local-only",
  "network-accessible",
]);
export const DesktopServerExposureManagementSchema = Schema.Literals(["native", "external"]);

export interface DesktopServerExposureState {
  mode: DesktopServerExposureMode;
  configuredMode: DesktopServerExposureMode;
  management: DesktopServerExposureManagement;
  endpointUrl: string | null;
  advertisedHost: string | null;
  tailscaleServeEnabled: boolean;
  tailscaleServePort: number;
}

export const DesktopServerExposureStateSchema = Schema.Struct({
  mode: DesktopServerExposureModeSchema,
  configuredMode: DesktopServerExposureModeSchema,
  management: DesktopServerExposureManagementSchema,
  endpointUrl: Schema.NullOr(Schema.String),
  advertisedHost: Schema.NullOr(Schema.String),
  tailscaleServeEnabled: Schema.Boolean,
  tailscaleServePort: Schema.Finite,
});

export interface PickFolderOptions {
  initialPath?: string | null;
  // When set, the desktop dialog opens against the named backend's
  // filesystem instead of the primary's. Used by callers that already
  // know which local environment they're targeting (e.g. opening a
  // project that lives inside WSL). Omitting it keeps the historical
  // behavior so non-WSL users never see a different picker.
  targetEnvironmentId?: string;
}

export const PickFolderOptionsSchema = Schema.Struct({
  initialPath: Schema.optionalKey(Schema.NullOr(Schema.String)),
  targetEnvironmentId: Schema.optionalKey(Schema.String),
});

export interface DesktopWslDistro {
  name: string;
  isDefault: boolean;
  version: 1 | 2;
}

export const DesktopWslDistroSchema = Schema.Struct({
  name: Schema.String,
  isDefault: Schema.Boolean,
  version: Schema.Literals([1, 2]),
});

export interface DesktopWslState {
  // True when the user has opted the WSL backend in; the actual backend
  // process is registered with the desktop pool independently of this
  // flag and may take a moment to come up after the user enables it.
  enabled: boolean;
  // null means "track the current WSL default distro".
  distro: string | null;
  available: boolean;
  // When true the desktop runs only the WSL backend as the primary and treats
  // this intent as authoritative even if a persisted `enabled` flag is stale.
  // The Windows-side backend is not started. Toggling this requires an app restart because the
  // primary backend's spec is captured once at layer init.
  wslOnly: boolean;
  distros: readonly DesktopWslDistro[];
  // A structured primary or secondary WSL failure. Primary failures never
  // substitute Windows; secondary failures leave the Windows primary live.
  // null means planning/startup has not failed.
  preflightError: DesktopWslPreflightError | null;
}

export const DesktopWslStateSchema = Schema.Struct({
  enabled: Schema.Boolean,
  distro: Schema.NullOr(Schema.String),
  available: Schema.Boolean,
  wslOnly: Schema.Boolean,
  distros: Schema.Array(DesktopWslDistroSchema),
  preflightError: Schema.NullOr(DesktopWslPreflightErrorSchema),
});

/**
 * Renderer-facing snapshot of a desktop preview tab. Mirrors the main-process
 * PreviewTabState shape but uses serialisable primitives only.
 */
export type DesktopPreviewNavStatus =
  | { kind: "Idle" }
  | { kind: "Loading"; url: string; title: string }
  | { kind: "Success"; url: string; title: string }
  | {
      kind: "LoadFailed";
      url: string;
      title: string;
      code: number;
      description: string;
    };

export interface DesktopPreviewTabState {
  tabId: string;
  webContentsId: number | null;
  navStatus: DesktopPreviewNavStatus;
  canGoBack: boolean;
  canGoForward: boolean;
  /** Current zoom factor (1.0 = 100%). */
  zoomFactor: number;
  controller: "human" | "agent" | "none";
  updatedAt: string;
}

export const DesktopPreviewTabIdSchema = Schema.String.check(Schema.isTrimmed()).check(
  Schema.isNonEmpty(),
);

export const DesktopPreviewNavStatusSchema = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("Idle") }),
  Schema.Struct({
    kind: Schema.Literal("Loading"),
    url: Schema.String,
    title: Schema.String,
  }),
  Schema.Struct({
    kind: Schema.Literal("Success"),
    url: Schema.String,
    title: Schema.String,
  }),
  Schema.Struct({
    kind: Schema.Literal("LoadFailed"),
    url: Schema.String,
    title: Schema.String,
    code: Schema.Finite,
    description: Schema.String,
  }),
]);

export const DesktopPreviewTabStateSchema: Schema.Codec<DesktopPreviewTabState> = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  webContentsId: Schema.NullOr(Schema.Int),
  navStatus: DesktopPreviewNavStatusSchema,
  canGoBack: Schema.Boolean,
  canGoForward: Schema.Boolean,
  zoomFactor: Schema.Finite,
  controller: Schema.Literals(["human", "agent", "none"]),
  updatedAt: Schema.String,
});

export interface DesktopPreviewPointerEvent {
  tabId: string;
  phase: "move" | "click";
  x: number;
  y: number;
  sequence: number;
  createdAt: string;
}

export const DesktopPreviewPointerEventSchema: Schema.Codec<DesktopPreviewPointerEvent> =
  Schema.Struct({
    tabId: DesktopPreviewTabIdSchema,
    phase: Schema.Literals(["move", "click"]),
    x: Schema.Finite,
    y: Schema.Finite,
    sequence: Schema.Int,
    createdAt: Schema.String,
  });

/**
 * Panel-relative bounds for a native preview webview, in logical (CSS)
 * pixels relative to the main window's client area. The renderer measures
 * these from `browserSurfaceStore` and forwards them verbatim.
 */
export interface DesktopPreviewBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export const DesktopPreviewBoundsSchema: Schema.Codec<DesktopPreviewBounds> = Schema.Struct({
  x: Schema.Finite,
  y: Schema.Finite,
  width: Schema.Finite.check(Schema.isGreaterThanOrEqualTo(0)),
  height: Schema.Finite.check(Schema.isGreaterThanOrEqualTo(0)),
});

/**
 * Static config a renderer needs to mount a preview `<webview>`. Returned
 * atomically by `DesktopPreviewBridge.getPreviewConfig()` so the renderer
 * doesn't have to wait on three separate IPC round-trips before the webview
 * can attach.
 */
export interface DesktopPreviewWebviewConfig {
  /** `persist:bibcode-preview` (or whatever the desktop chose). */
  partition: string;
  /**
   * Canonical `<webview webpreferences="...">` string. Encodes the security
   * posture (sandboxed but contextIsolation off so the picker preload can
   * read the page's React DevTools hook). Always present.
   */
  webPreferences: string;
  /**
   * Absolute `file://`-style URL to the picker preload bundle. Set to null
   * when the bundle isn't present (older builds, broken install) — the
   * renderer must then disable element-pick affordances.
   */
  preloadUrl: string | null;
}

export const DesktopPreviewWebviewConfigSchema: Schema.Codec<DesktopPreviewWebviewConfig> =
  Schema.Struct({
    partition: Schema.String,
    webPreferences: Schema.String,
    preloadUrl: Schema.NullOr(Schema.String),
  });

export interface DesktopPreviewAnnotationTheme {
  colorScheme: "light" | "dark";
  radius: string;
  background: string;
  foreground: string;
  popover: string;
  popoverForeground: string;
  primary: string;
  primaryForeground: string;
  muted: string;
  mutedForeground: string;
  accent: string;
  accentForeground: string;
  border: string;
  input: string;
  ring: string;
  fontSans: string;
  fontMono: string;
}

export const DesktopPreviewAnnotationThemeSchema: Schema.Codec<DesktopPreviewAnnotationTheme> =
  Schema.Struct({
    colorScheme: Schema.Literals(["light", "dark"]),
    radius: Schema.String,
    background: Schema.String,
    foreground: Schema.String,
    popover: Schema.String,
    popoverForeground: Schema.String,
    primary: Schema.String,
    primaryForeground: Schema.String,
    muted: Schema.String,
    mutedForeground: Schema.String,
    accent: Schema.String,
    accentForeground: Schema.String,
    border: Schema.String,
    input: Schema.String,
    ring: Schema.String,
    fontSans: Schema.String,
    fontMono: Schema.String,
  });

export interface DesktopPreviewRecordingFrame {
  tabId: string;
  data: string;
  width: number;
  height: number;
  receivedAt: string;
}

export const DesktopPreviewRecordingFrameSchema: Schema.Codec<DesktopPreviewRecordingFrame> =
  Schema.Struct({
    tabId: DesktopPreviewTabIdSchema,
    data: Schema.String,
    width: Schema.Finite,
    height: Schema.Finite,
    receivedAt: Schema.String,
  });

export interface DesktopPreviewRecordingArtifact {
  id: string;
  tabId: string;
  path: string;
  mimeType: string;
  sizeBytes: number;
  createdAt: string;
}

export const DesktopPreviewRecordingArtifactSchema: Schema.Codec<DesktopPreviewRecordingArtifact> =
  Schema.Struct({
    id: Schema.String,
    tabId: DesktopPreviewTabIdSchema,
    path: Schema.String,
    mimeType: Schema.String,
    sizeBytes: Schema.Int,
    createdAt: Schema.String,
  });

export interface DesktopPreviewScreenshotArtifact {
  id: string;
  tabId: string;
  path: string;
  mimeType: "image/png";
  sizeBytes: number;
  createdAt: string;
}

export const DesktopPreviewScreenshotArtifactSchema: Schema.Codec<DesktopPreviewScreenshotArtifact> =
  Schema.Struct({
    id: Schema.String,
    tabId: DesktopPreviewTabIdSchema,
    path: Schema.String,
    mimeType: Schema.Literal("image/png"),
    sizeBytes: Schema.Int,
    createdAt: Schema.String,
  });

/**
 * Single stack frame captured by react-grab's `getElementContext`. We surface
 * the source file/line so coding agents can jump straight to the JSX that
 * produced the picked DOM node.
 */
export interface PickedElementStackFrame {
  functionName: string | null;
  fileName: string | null;
  lineNumber: number | null;
  columnNumber: number | null;
}

export const PickedElementStackFrameSchema: Schema.Codec<PickedElementStackFrame> = Schema.Struct({
  functionName: Schema.NullOr(Schema.String),
  fileName: Schema.NullOr(Schema.String),
  lineNumber: Schema.NullOr(Schema.Finite),
  columnNumber: Schema.NullOr(Schema.Finite),
});

/**
 * A successful element pick from the preview webview. All fields are
 * best-effort — pages that don't ship a React fiber tree (or aren't running
 * in dev) will still produce a usable payload (selector + html preview),
 * just without component / source attribution.
 */
export interface PickedElementPayload {
  /** URL of the page the element was picked on. */
  pageUrl: string;
  /** Optional `<title>` of that page (best-effort). */
  pageTitle: string | null;
  /** Lowercase tag name, e.g. `"button"`. */
  tagName: string;
  /** CSS selector resolving back to the element on a re-render. */
  selector: string | null;
  /** Truncated outer-HTML preview (matches react-grab's `htmlPreview`). */
  htmlPreview: string;
  /** Nearest React component display name, or null when unavailable. */
  componentName: string | null;
  /** First source-mapped stack frame (file + line of the JSX source). */
  source: PickedElementStackFrame | null;
  /** Full owner-stack frames; can be empty. Useful for richer context. */
  stack: ReadonlyArray<PickedElementStackFrame>;
  /** Author CSS only (UA defaults stripped) — react-grab's `styles`. */
  styles: string;
  /** Wall-clock pick time as ISO-8601 string. */
  pickedAt: string;
}

export const PickedElementPayloadSchema: Schema.Codec<PickedElementPayload> = Schema.Struct({
  pageUrl: Schema.String,
  pageTitle: Schema.NullOr(Schema.String),
  tagName: Schema.String,
  selector: Schema.NullOr(Schema.String),
  htmlPreview: Schema.String,
  componentName: Schema.NullOr(Schema.String),
  source: Schema.NullOr(PickedElementStackFrameSchema),
  stack: Schema.Array(PickedElementStackFrameSchema),
  styles: Schema.String,
  pickedAt: Schema.String,
});

export interface PreviewAnnotationRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export const PreviewAnnotationRectSchema: Schema.Codec<PreviewAnnotationRect> = Schema.Struct({
  x: Schema.Finite,
  y: Schema.Finite,
  width: Schema.Finite,
  height: Schema.Finite,
});

export interface PreviewAnnotationPoint {
  x: number;
  y: number;
}

export const PreviewAnnotationPointSchema: Schema.Codec<PreviewAnnotationPoint> = Schema.Struct({
  x: Schema.Finite,
  y: Schema.Finite,
});

export interface PreviewAnnotationElementTarget {
  id: string;
  element: PickedElementPayload;
  rect: PreviewAnnotationRect;
}

export const PreviewAnnotationElementTargetSchema: Schema.Codec<PreviewAnnotationElementTarget> =
  Schema.Struct({
    id: Schema.String,
    element: PickedElementPayloadSchema,
    rect: PreviewAnnotationRectSchema,
  });

export interface PreviewAnnotationRegionTarget {
  id: string;
  rect: PreviewAnnotationRect;
}

export const PreviewAnnotationRegionTargetSchema: Schema.Codec<PreviewAnnotationRegionTarget> =
  Schema.Struct({
    id: Schema.String,
    rect: PreviewAnnotationRectSchema,
  });

export interface PreviewAnnotationStrokeTarget {
  id: string;
  color: string;
  width: number;
  points: ReadonlyArray<PreviewAnnotationPoint>;
  bounds: PreviewAnnotationRect;
}

export const PreviewAnnotationStrokeTargetSchema: Schema.Codec<PreviewAnnotationStrokeTarget> =
  Schema.Struct({
    id: Schema.String,
    color: Schema.String,
    width: Schema.Finite,
    points: Schema.Array(PreviewAnnotationPointSchema),
    bounds: PreviewAnnotationRectSchema,
  });

export interface PreviewAnnotationStyleChange {
  targetId: string;
  selector: string | null;
  property: string;
  previousValue: string;
  value: string;
}

export const PreviewAnnotationStyleChangeSchema: Schema.Codec<PreviewAnnotationStyleChange> =
  Schema.Struct({
    targetId: Schema.String,
    selector: Schema.NullOr(Schema.String),
    property: Schema.String,
    previousValue: Schema.String,
    value: Schema.String,
  });

export interface PreviewAnnotationScreenshot {
  dataUrl: string;
  width: number;
  height: number;
  cropRect: PreviewAnnotationRect;
}

export const PreviewAnnotationScreenshotSchema: Schema.Codec<PreviewAnnotationScreenshot> =
  Schema.Struct({
    dataUrl: Schema.String,
    width: Schema.Finite,
    height: Schema.Finite,
    cropRect: PreviewAnnotationRectSchema,
  });

/**
 * A submitted preview annotation. One annotation may reference multiple DOM
 * elements, freeform regions, and ink strokes. The desktop main process adds
 * the screenshot after the guest preload submits the structured draft.
 */
export interface PreviewAnnotationPayload {
  id: string;
  pageUrl: string;
  pageTitle: string | null;
  comment: string;
  elements: ReadonlyArray<PreviewAnnotationElementTarget>;
  regions: ReadonlyArray<PreviewAnnotationRegionTarget>;
  strokes: ReadonlyArray<PreviewAnnotationStrokeTarget>;
  styleChanges: ReadonlyArray<PreviewAnnotationStyleChange>;
  screenshot: PreviewAnnotationScreenshot | null;
  createdAt: string;
}

export const PreviewAnnotationPayloadSchema: Schema.Codec<PreviewAnnotationPayload> = Schema.Struct(
  {
    id: Schema.String,
    pageUrl: Schema.String,
    pageTitle: Schema.NullOr(Schema.String),
    comment: Schema.String,
    elements: Schema.Array(PreviewAnnotationElementTargetSchema),
    regions: Schema.Array(PreviewAnnotationRegionTargetSchema),
    strokes: Schema.Array(PreviewAnnotationStrokeTargetSchema),
    styleChanges: Schema.Array(PreviewAnnotationStyleChangeSchema),
    screenshot: Schema.NullOr(PreviewAnnotationScreenshotSchema),
    createdAt: Schema.String,
  },
);

export const DesktopPreviewTabInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
});

export const DesktopPreviewRegisterWebviewInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  webContentsId: Schema.Int.check(Schema.isGreaterThan(0)),
});

export const DesktopPreviewNavigateInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  url: Schema.String,
});

export const DesktopPreviewConfigInputSchema = Schema.Struct({
  environmentId: EnvironmentId,
});

export const DesktopPreviewAnnotationThemeInputSchema = Schema.Struct({
  theme: DesktopPreviewAnnotationThemeSchema,
});

export const DesktopPreviewArtifactInputSchema = Schema.Struct({
  path: Schema.String.check(Schema.isTrimmed()).check(Schema.isNonEmpty()),
});

export const DesktopPreviewRecordingSaveInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  mimeType: Schema.String.check(Schema.isTrimmed()).check(Schema.isNonEmpty()),
  data: Schema.Uint8Array,
});

export const DesktopPreviewAutomationClickInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  input: PreviewAutomationClickInput,
});

export const DesktopPreviewAutomationTypeInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  input: PreviewAutomationTypeInput,
});

export const DesktopPreviewAutomationPressInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  input: PreviewAutomationPressInput,
});

export const DesktopPreviewAutomationScrollInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  input: PreviewAutomationScrollInput,
});

export const DesktopPreviewAutomationEvaluateInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  input: PreviewAutomationEvaluateInput,
});

export const DesktopPreviewAutomationWaitForInputSchema = Schema.Struct({
  tabId: DesktopPreviewTabIdSchema,
  input: PreviewAutomationWaitForInput,
});

/** Tauri deep-link plugin runtime event carrying the opened URLs. */
export const DESKTOP_DEEP_LINK_EVENT = "deep-link://new-url";

export interface DesktopBridge {
  getHostMetadata?: () => Promise<DesktopBridgeHostMetadata>;
  getAppBranding: () => DesktopAppBranding | null;
  // One bootstrap per pool instance currently registered with bootstrap
  // info (omits instances whose backend hasn't produced a config yet).
  // The primary backend is identified by id === PRIMARY_LOCAL_ENVIRONMENT_ID.
  getLocalEnvironmentBootstraps: () => readonly DesktopEnvironmentBootstrap[];
  getLocalEnvironmentBearerToken: () => Promise<string>;
  getClientSettings: () => Promise<ClientSettings | null>;
  setClientSettings: (settings: ClientSettings) => Promise<void>;
  getConnectionCatalog?: () => Promise<string | null>;
  setConnectionCatalog?: (catalog: string) => Promise<boolean>;
  compareAndSetConnectionCatalog?: (
    expectedCatalog: string | null,
    nextCatalog: string,
  ) => Promise<boolean>;
  compareConnectionCatalog?: (expectedCatalog: string | null) => Promise<boolean>;
  clearConnectionCatalog?: () => Promise<void>;
  getProjectDataStatuses?: () => Promise<readonly DesktopProjectDataEnvironmentStatus[]>;
  onProjectDataStatusChanged?: (
    listener: (event: DesktopProjectDataStatusChangedEvent) => void,
  ) => () => void;
  /** URLs the OS handed to the app before the webview subscribed. */
  getPendingDeepLinks?: () => Promise<ReadonlyArray<string>>;
  /** Subscribe to bibcode:// URLs opened while the app is running. */
  onDeepLink?: (listener: (urls: ReadonlyArray<string>) => void) => () => void;
  restoreProjectData?: (
    environmentId: string,
    backupId: string,
  ) => Promise<DesktopProjectDataRecoveryResult>;
  startEmptyProjectData?: (environmentId: string) => Promise<DesktopProjectDataRecoveryResult>;
  retryProjectData?: (environmentId: string) => Promise<void>;
  restartApp?: () => Promise<void>;
  openProjectDataPath?: (environmentId: string) => Promise<void>;
  exportProjectDataDiagnostics?: (environmentId: string) => Promise<string | null>;
  discoverSshHosts: () => Promise<readonly DesktopDiscoveredSshHost[]>;
  ensureSshEnvironment: (
    target: DesktopSshEnvironmentTarget,
    options?: { issuePairingToken?: boolean },
  ) => Promise<DesktopSshEnvironmentBootstrap>;
  disconnectSshEnvironment: (target: DesktopSshEnvironmentTarget) => Promise<void>;
  /**
   * Forwards `remotePort` on the target's loopback over its live SSH
   * connection and resolves the local port. Idempotent per remote port. The
   * forward ends when the connection ends or reconnects, on
   * `releaseSshForward`, on LRU eviction when its connection holds more than
   * 8 forwards, or on budget eviction when all connections hold more than 12.
   * Eviction takes the least recently used forward, and only after the new
   * one is ready; a later call for the same port opens a new forward.
   */
  sshForward: (target: DesktopSshEnvironmentTarget, remotePort: number) => Promise<number>;
  releaseSshForward: (target: DesktopSshEnvironmentTarget, remotePort: number) => Promise<void>;
  fetchSshEnvironmentDescriptor: (httpBaseUrl: string) => Promise<ExecutionEnvironmentDescriptor>;
  /**
   * Exchanges a one-time SSH pairing credential for a bearer. `scopes` is
   * sent as the OAuth `scope`; without it the host grants every scope the
   * bootstrap allows, administrative ones included.
   */
  bootstrapSshBearerSession: (
    httpBaseUrl: string,
    credential: string,
    scopes?: ReadonlyArray<AuthEnvironmentScope>,
  ) => Promise<AuthAccessTokenResult>;
  fetchSshSessionState: (httpBaseUrl: string, bearerToken: string) => Promise<AuthSessionState>;
  issueSshWebSocketTicket: (
    httpBaseUrl: string,
    bearerToken: string,
  ) => Promise<AuthWebSocketTicketResult>;
  onSshPasswordPrompt: (listener: (request: DesktopSshPasswordPromptRequest) => void) => () => void;
  resolveSshPasswordPrompt: (requestId: string, password: string | null) => Promise<void>;
  getServerExposureState: () => Promise<DesktopServerExposureState>;
  applyServerExposure: (desired: DesktopServerExposureMode) => Promise<DesktopServerExposureState>;
  setTailscaleServeEnabled: (input: {
    readonly enabled: boolean;
    readonly port?: number;
  }) => Promise<DesktopServerExposureState>;
  getAdvertisedEndpoints: () => Promise<readonly AdvertisedEndpoint[]>;
  getWslState: () => Promise<DesktopWslState>;
  setWslBackendEnabled: (enabled: boolean) => Promise<DesktopWslState>;
  setWslDistro: (distro: string | null) => Promise<DesktopWslState>;
  setWslOnly: (enabled: boolean) => Promise<DesktopWslState>;
  pickFolder: (options?: PickFolderOptions) => Promise<string | null>;
  saveDiagnosticLogs?: (filename: string, bytes: Uint8Array) => Promise<string | null>;
  pickFiles?: (options?: { title?: string }) => Promise<readonly string[]>;
  downloadToFolder?: (input: {
    url: string;
    directory: string;
    fileName: string;
  }) => Promise<string>;
  uploadFile?: (input: { url: string; path: string }) => Promise<{ status: number; body: string }>;
  confirm: (message: string) => Promise<boolean>;
  setTheme: (theme: DesktopTheme) => Promise<void>;
  showContextMenu: <T extends string>(
    items: readonly ContextMenuEntry<T>[],
    position?: { x: number; y: number },
  ) => Promise<T | null>;
  openExternal: (url: string) => Promise<boolean>;
  openInFileManager?: (path: string, isDirectory: boolean) => Promise<void>;
  onMenuAction: (listener: (action: string) => void) => () => void;
  getUpdateState: () => Promise<DesktopUpdateState>;
  checkForUpdate: () => Promise<DesktopUpdateCheckResult>;
  downloadUpdate: () => Promise<DesktopUpdateActionResult>;
  installUpdate: (input?: DesktopUpdateInstallInput) => Promise<DesktopUpdateActionResult>;
  onUpdateState: (listener: (state: DesktopUpdateState) => void) => () => void;
  /**
   * Desktop-only preview surface. Present iff the renderer is hosted by the
   * native preview host. Tauri installs this bridge; web builds leave
   * `preview === undefined`.
   */
  preview?: DesktopPreviewBridge;
}

export interface DesktopPreviewBridge {
  createTab: (tabId: string) => Promise<void>;
  closeTab: (tabId: string) => Promise<void>;
  /**
   * Electron-era API: associate a renderer-mounted `<webview>`. Absent on
   * the Tauri host, which owns webview lifetime natively.
   */
  registerWebview?: (tabId: string, webContentsId: number) => Promise<void>;
  /**
   * Position/show the native webview over the right-panel rect. Bounds are
   * logical pixels relative to the main window's client area. `visible:
   * false` hides the webview without destroying it (tab switched away).
   */
  setBounds: (tabId: string, bounds: DesktopPreviewBounds, visible: boolean) => Promise<void>;
  navigate: (tabId: string, url: string) => Promise<void>;
  goBack: (tabId: string) => Promise<void>;
  goForward: (tabId: string) => Promise<void>;
  refresh: (tabId: string) => Promise<void>;
  zoomIn: (tabId: string) => Promise<void>;
  zoomOut: (tabId: string) => Promise<void>;
  resetZoom: (tabId: string) => Promise<void>;
  /** Reload bypassing the HTTP cache. */
  hardReload: (tabId: string) => Promise<void>;
  /** Open the guest webview's DevTools (detached). */
  openDevTools: (tabId: string) => Promise<void>;
  /** Drop cookies + storage data for the preview partition (all tabs). */
  clearCookies: () => Promise<void>;
  /** Drop the HTTP cache for the preview partition (all tabs). */
  clearCache: () => Promise<void>;
  /**
   * Electron-era one-shot config for mounting a preview `<webview>`. Absent
   * on the Tauri host, which owns webview configuration natively. Replaces three
   * earlier round-trip calls (`getBrowserPartition`, `getWebviewPreferences`,
   * `getPickPreloadPath`) so adding a new field here only requires touching
   * the contract + main, not the renderer's mount logic.
   */
  getPreviewConfig?: (environmentId: EnvironmentId) => Promise<DesktopPreviewWebviewConfig>;
  setAnnotationTheme: (theme: DesktopPreviewAnnotationTheme) => Promise<void>;
  /**
   * Activate the in-page element picker for the given tab. Resolves with
   * the picked payload, or `null` when the user cancels (Escape / nav). The
   * promise rejects if the picker can't be activated (no webview, etc.).
   */
  pickElement: (tabId: string) => Promise<PreviewAnnotationPayload | null>;
  /** Cancel an in-flight preview annotation session. */
  cancelPickElement: (tabId: string) => Promise<void>;
  captureScreenshot: (tabId: string) => Promise<DesktopPreviewScreenshotArtifact>;
  revealArtifact: (path: string) => Promise<void>;
  copyArtifactToClipboard: (path: string) => Promise<void>;
  recording: {
    startScreencast: (tabId: string) => Promise<void>;
    stopScreencast: (tabId: string) => Promise<void>;
    save: (
      tabId: string,
      mimeType: string,
      data: Uint8Array,
    ) => Promise<DesktopPreviewRecordingArtifact>;
    onFrame: (listener: (frame: DesktopPreviewRecordingFrame) => void) => () => void;
  };
  automation: {
    status: (tabId: string) => Promise<PreviewAutomationStatus>;
    snapshot: (tabId: string) => Promise<PreviewAutomationSnapshot>;
    click: (tabId: string, input: PreviewAutomationClickInput) => Promise<void>;
    type: (tabId: string, input: PreviewAutomationTypeInput) => Promise<void>;
    press: (tabId: string, input: PreviewAutomationPressInput) => Promise<void>;
    scroll: (tabId: string, input: PreviewAutomationScrollInput) => Promise<void>;
    evaluate: (tabId: string, input: PreviewAutomationEvaluateInput) => Promise<unknown>;
    waitFor: (tabId: string, input: PreviewAutomationWaitForInput) => Promise<void>;
  };
  onStateChange: (listener: (tabId: string, state: DesktopPreviewTabState) => void) => () => void;
  /** A page in a preview tab asked to open a new window (popup or target=_blank). */
  onNewWindowRequest: (listener: (tabId: string, url: string) => void) => () => void;
  onPointerEvent: (listener: (event: DesktopPreviewPointerEvent) => void) => () => void;
}

/**
 * APIs bound to the local app shell, not to any particular backend environment.
 *
 * These capabilities describe the desktop/browser host that the user is
 * currently running: dialogs, editor/external-link opening, context menus, and
 * app-level settings/config access. They must not be used as a proxy for
 * "whatever environment the user is targeting", because in a multi-environment
 * world the local shell and a selected backend environment are distinct
 * concepts.
 */
export interface LocalApi {
  dialogs: {
    pickFolder: (options?: PickFolderOptions) => Promise<string | null>;
    confirm: (message: string) => Promise<boolean>;
  };
  shell: {
    openInEditor: (cwd: string, editor: EditorId) => Promise<void>;
    openExternal: (url: string) => Promise<void>;
  };
  contextMenu: {
    show: <T extends string>(
      items: readonly ContextMenuEntry<T>[],
      position?: { x: number; y: number },
    ) => Promise<T | null>;
  };
  persistence: {
    getClientSettings: () => Promise<ClientSettings | null>;
    setClientSettings: (settings: ClientSettings) => Promise<void>;
  };
  server: {
    getConfig: () => Promise<ServerConfig>;
    /**
     * Refresh provider snapshots. When `input.instanceId` is supplied only that
     * configured instance is probed; otherwise every configured instance is
     * refreshed (legacy untargeted refresh).
     */
    refreshProviders: (input?: {
      readonly instanceId?: ProviderInstanceId;
    }) => Promise<ServerProviderUpdatedPayload>;
    updateProvider: (input: ServerProviderUpdateInput) => Promise<ServerProviderUpdatedPayload>;
    upsertKeybinding: (input: ServerUpsertKeybindingInput) => Promise<ServerUpsertKeybindingResult>;
    removeKeybinding: (input: ServerRemoveKeybindingInput) => Promise<ServerRemoveKeybindingResult>;
    getSettings: () => Promise<ServerSettings>;
    updateSettings: (patch: ServerSettingsPatch) => Promise<ServerSettings>;
    discoverSourceControl: () => Promise<SourceControlDiscoveryResult>;
    getTraceDiagnostics: () => Promise<ServerTraceDiagnosticsResult>;
    getProcessDiagnostics: () => Promise<ServerProcessDiagnosticsResult>;
    getProcessResourceHistory: (
      input: ServerProcessResourceHistoryInput,
    ) => Promise<ServerProcessResourceHistoryResult>;
    getProfilerStatus: () => Promise<ProfilerStatus>;
    reportProfilerClientBatch: (input: ProfilerClientBatch) => Promise<ProfilerClientBatchResult>;
    signalProcess: (input: ServerSignalProcessInput) => Promise<ServerSignalProcessResult>;
  };
}

/**
 * APIs bound to a specific backend environment connection.
 *
 * These operations must always be routed with explicit environment context.
 * They represent remote stateful capabilities such as orchestration, terminal,
 * project, VCS, and provider operations. In multi-environment mode, each environment gets
 * its own instance of this surface, and callers should resolve it by
 * `environmentId` rather than reaching through the local desktop bridge.
 */
export interface EnvironmentApi {
  terminal: {
    open: (input: typeof TerminalOpenInput.Encoded) => Promise<TerminalSessionSnapshot>;
    attach: (
      input: typeof TerminalAttachInput.Encoded,
      callback: (event: TerminalAttachStreamEvent) => void,
      options?: {
        onResubscribe?: () => void;
      },
    ) => () => void;
    write: (input: typeof TerminalWriteInput.Encoded) => Promise<void>;
    resize: (input: typeof TerminalResizeInput.Encoded) => Promise<void>;
    clear: (input: typeof TerminalClearInput.Encoded) => Promise<void>;
    restart: (input: typeof TerminalRestartInput.Encoded) => Promise<TerminalSessionSnapshot>;
    close: (input: typeof TerminalCloseInput.Encoded) => Promise<void>;
    onMetadata: (
      callback: (event: TerminalMetadataStreamEvent) => void,
      options?: {
        onResubscribe?: () => void;
      },
    ) => () => void;
  };
  projects: {
    listEntries: (input: ProjectListEntriesInput) => Promise<ProjectListEntriesResult>;
    readFile: (input: ProjectReadFileInput) => Promise<ProjectReadFileResult>;
    searchEntries: (input: ProjectSearchEntriesInput) => Promise<ProjectSearchEntriesResult>;
    writeFile: (input: ProjectWriteFileInput) => Promise<ProjectWriteFileResult>;
  };
  filesystem: {
    browse: (input: FilesystemBrowseInput) => Promise<FilesystemBrowseResult>;
  };
  assets: {
    createUrl: (input: AssetCreateUrlInput) => Promise<AssetCreateUrlResult>;
  };
  sourceControl: {
    lookupRepository: (
      input: SourceControlRepositoryLookupInput,
    ) => Promise<SourceControlRepositoryInfo>;
    cloneRepository: (
      input: SourceControlCloneRepositoryInput,
    ) => Promise<SourceControlCloneRepositoryResult>;
    publishRepository: (
      input: SourceControlPublishRepositoryInput,
    ) => Promise<SourceControlPublishRepositoryResult>;
  };
  vcs: {
    listRefs: (input: VcsListRefsInput) => Promise<VcsListRefsResult>;
    createRef: (input: VcsCreateRefInput) => Promise<VcsCreateRefResult>;
    switchRef: (input: VcsSwitchRefInput) => Promise<VcsSwitchRefResult>;
    init: (input: VcsInitInput) => Promise<void>;
    pull: (input: VcsPullInput) => Promise<VcsPullResult>;
    refreshStatus: (input: VcsStatusInput) => Promise<VcsStatusResult>;
    onStatus: (
      input: VcsStatusInput,
      callback: (status: VcsStatusResult) => void,
      options?: {
        onResubscribe?: () => void;
      },
    ) => () => void;
  };
  git: {
    resolvePullRequest: (input: GitPullRequestRefInput) => Promise<GitResolvePullRequestResult>;
    preparePullRequestThread: (
      input: GitPreparePullRequestThreadInput,
    ) => Promise<GitPreparePullRequestThreadResult>;
  };
  review: {
    getDiffPreview: (input: ReviewDiffPreviewInput) => Promise<ReviewDiffPreviewResult>;
  };
  orchestration: {
    dispatchCommand: (command: ClientOrchestrationCommand) => Promise<DispatchResult>;
    getTurnDiff: (input: OrchestrationGetTurnDiffInput) => Promise<OrchestrationGetTurnDiffResult>;
    getFullThreadDiff: (
      input: OrchestrationGetFullThreadDiffInput,
    ) => Promise<OrchestrationGetFullThreadDiffResult>;
    getArchivedShellSnapshot: () => Promise<OrchestrationShellSnapshot>;
    subscribeShell: (
      callback: (event: OrchestrationShellStreamItem) => void,
      options?: {
        onResubscribe?: () => void;
      },
    ) => () => void;
    subscribeThread: (
      input: OrchestrationSubscribeThreadInput,
      callback: (event: OrchestrationThreadStreamItem) => void,
      options?: {
        onResubscribe?: () => void;
      },
    ) => () => void;
  };
  preview: {
    open: (input: typeof PreviewOpenInput.Encoded) => Promise<PreviewSessionSnapshot>;
    navigate: (input: typeof PreviewNavigateInput.Encoded) => Promise<PreviewSessionSnapshot>;
    resize: (input: typeof PreviewResizeInput.Encoded) => Promise<PreviewSessionSnapshot>;
    refresh: (input: typeof PreviewRefreshInput.Encoded) => Promise<void>;
    close: (input: typeof PreviewCloseInput.Encoded) => Promise<void>;
    list: (input: typeof PreviewListInput.Encoded) => Promise<PreviewListResult>;
    reportStatus: (input: typeof PreviewReportStatusInput.Encoded) => Promise<void>;
    automation: {
      connect: (
        input: PreviewAutomationHost,
        callback: (event: PreviewAutomationStreamEvent) => void,
        options?: { onResubscribe?: () => void },
      ) => () => void;
      respond: (response: PreviewAutomationResponse) => Promise<void>;
      focusHost: (input: PreviewAutomationHostFocus) => Promise<void>;
    };
    onEvent: (
      callback: (event: PreviewEvent) => void,
      options?: { onResubscribe?: () => void },
    ) => () => void;
    subscribePorts: (
      callback: (servers: DiscoveredLocalServerList) => void,
      options?: { onResubscribe?: () => void },
    ) => () => void;
  };
}
