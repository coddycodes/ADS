import {
  UploadBeginInput,
  UploadBeginResult,
  UploadAppendInput,
  UploadAppendResult,
  UploadGetInput,
  UploadGetResult,
  UploadCancelInput,
  UploadCancelResult,
  UploadError,
  TerminalStageImagePasteInput,
  TerminalStageImagePasteResult,
} from "./uploads.ts";
import * as Schema from "effect/Schema";
import {
  DeviceCatalog,
  DeviceIdInput,
  DeviceInputInput,
  DeviceJpegFrame,
  DeviceOpenInput,
  DeviceOpenResult,
  DeviceRotateInput,
  DeviceRpcError,
} from "./device.ts";
import * as Rpc from "effect/unstable/rpc/Rpc";
import * as RpcGroup from "effect/unstable/rpc/RpcGroup";

import {
  PullRequestsActionRequest,
  PullRequestsActionResult,
  PullRequestsCheckoutInput,
  PullRequestsCheckoutResult,
  PullRequestsChecks,
  PullRequestsCommits,
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsFiles,
  PullRequestsGetContextInput,
  PullRequestsListInput,
  PullRequestsListPage,
  PullRequestsNumberInput,
  PullRequestsOperationError,
  PullRequestsTimeline,
  PullRequestsVocabulary,
  PullRequestsVocabularyInput,
  PullRequestsCwdInput,
  PullRequestsCreateDefaults,
  PullRequestsChanged,
  PullRequestsSnapshot,
  PullRequestsSubscribeInput,
} from "./pullRequests.ts";

import { ExternalLauncherError, LaunchEditorInput } from "./editor.ts";
import {
  AgentSessionImportInput,
  AgentSessionImportResult,
  AgentSessionScanInput,
  AgentSessionScanResult,
  AgentSessionsError,
} from "./agentSessions.ts";
import {
  AuthAccessStreamError,
  AuthAccessStreamEvent,
  EnvironmentAuthorizationError,
  EnvironmentRpcError,
} from "./auth.ts";
import {
  ActivityCancelSubtreeInput,
  ActivityDetailPage,
  ActivityError,
  ActivityGetSnapshotInput,
  ActivityListDetailInput,
  ActivityListRosterInput,
  ActivityRosterPage,
  ActivityRetrySubtreeCancellationInput,
  ActivityScopeRef,
  ActivitySnapshot,
  ActivityStreamItem,
  ActivitySubtreeCancellationResult,
} from "./activity.ts";
import {
  FilesystemBrowseInput,
  FilesystemBrowseResult,
  FilesystemBrowseError,
} from "./filesystem.ts";
import { AssetAccessError, AssetCreateUrlInput, AssetCreateUrlResult } from "./assets.ts";
import {
  ProjectCreateDownloadUrlInput,
  ProjectCreateDownloadUrlResult,
  ProjectCreateUploadUrlInput,
  ProjectCreateUploadUrlResult,
  ProjectTransferError,
} from "./transfer.ts";
import {
  GitActionProgressEvent,
  VcsSwitchRefInput,
  VcsSwitchRefResult,
  GitCommandError,
  VcsCreateRefInput,
  VcsCreateRefResult,
  GitCancelCloneInput,
  GitCancelCloneResult,
  GitCloneInput,
  GitCloneOperationError,
  GitCloneResult,
  VcsDiscardFilesInput,
  VcsGenerateCommitMessageInput,
  VcsGenerateCommitMessageResult,
  VcsInitInput,
  VcsListCommitsInput,
  VcsListCommitsResult,
  VcsListRefsInput,
  VcsListRefsResult,
  GitManagerServiceError,
  GitPreparePullRequestThreadInput,
  GitPreparePullRequestThreadResult,
  VcsPullInput,
  GitPullRequestRefInput,
  VcsPullResult,
  GitResolvePullRequestResult,
  GitRunStackedActionInput,
  VcsStageFilesInput,
  VcsStatusInput,
  VcsStatusResult,
  VcsStatusStreamEvent,
  VcsUnstageFilesInput,
} from "./git.ts";
import {
  GitManagerCommitInput,
  GitManagerCommitPage,
  GitManagerCommitResult,
  GitManagerCwdInput,
  GitManagerDiff,
  GitManagerDiscardInput,
  GitManagerDiscardResult,
  GitManagerGetCommitsInput,
  GitManagerGetDiffInput,
  GitManagerMergePreview,
  GitManagerOperationError,
  GitManagerOperationEvent,
  GitManagerOperationRequest,
  GitManagerPartialSelectionInput,
  GitManagerPartialSelectionResult,
  GitManagerPreviewMergeInput,
  GitManagerPullRequestsResult,
  GitManagerRefsSnapshot,
  GitManagerRemoteTags,
  GitManagerRemoteTagsInput,
  GitManagerSignalEvent,
  GitManagerStashEntry,
  GitManagerUndoCommitResult,
} from "./gitManager.ts";
import {
  ReviewDiffPreviewError,
  ReviewDiffPreviewInput,
  ReviewDiffPreviewResult,
} from "./review.ts";
import { KeybindingsConfigError } from "./keybindings.ts";
import {
  ClientOrchestrationCommand,
  ModelSelection,
  ORCHESTRATION_WS_METHODS,
  OrchestrationDispatchCommandError,
  OrchestrationGetFullThreadDiffError,
  OrchestrationGetFullThreadDiffInput,
  OrchestrationGetSnapshotError,
  OrchestrationGetTurnDiffError,
  OrchestrationGetTurnDiffInput,
  OrchestrationReplayEventsError,
  OrchestrationReplayEventsInput,
  OrchestrationRpcSchemas,
  ProviderInteractionMode,
  RuntimeMode,
} from "./orchestration.ts";
import {
  CommandId,
  NonNegativeInt,
  ProjectId,
  ThreadId,
  TrimmedNonEmptyString,
} from "./baseSchemas.ts";
import { ProviderInstanceId } from "./providerInstance.ts";
import {
  RemoteUpdateActiveWork,
  RemoteUpdateActiveWorkError,
  RemoteUpdateInstallError,
  RemoteUpdateSnapshot,
} from "./remoteUpdate.ts";
import {
  RelayClientInstallFailedError,
  RelayClientInstallProgressEventSchema,
  RelayClientStatusSchema,
} from "./relayClient.ts";
import {
  ProjectCreateEntryError,
  ProjectCreateEntryInput,
  ProjectCreateEntryResult,
  ProjectDeleteEntryError,
  ProjectDeleteEntryInput,
  ProjectDeleteEntryResult,
  ProjectDuplicateEntryError,
  ProjectDuplicateEntryInput,
  ProjectDuplicateEntryResult,
  ProjectEntriesChangedEvent,
  ProjectListEntriesError,
  ProjectListEntriesInput,
  ProjectListEntriesResult,
  ProjectReadFileError,
  ProjectReadFileInput,
  ProjectReadFileResult,
  ProjectRenameEntryError,
  ProjectRenameEntryInput,
  ProjectRenameEntryResult,
  ProjectSearchEntriesError,
  ProjectSearchEntriesInput,
  ProjectSearchEntriesResult,
  ProjectSubscribeEntriesInput,
  ProjectWriteFileError,
  ProjectWriteFileInput,
  ProjectWriteFileResult,
} from "./project.ts";
import {
  TerminalAttachInput,
  TerminalAttachStreamEvent,
  TerminalClearInput,
  TerminalCloseInput,
  TerminalError,
  TerminalEvent,
  TerminalMetadataStreamEvent,
  TerminalOpenInput,
  TerminalResizeInput,
  TerminalRestartInput,
  TerminalSessionSnapshot,
  TerminalWriteInput,
  TerminalBeginInput,
  TerminalCancelInput,
  TerminalInputAcknowledgement,
  TerminalInputError,
  TerminalInputLease,
  TerminalWriteInputFrame,
} from "./terminal.ts";
import {
  DiscoveredLocalServerList,
  PreviewClaimOpenRequestInput,
  PreviewClaimOpenRequestResult,
  PreviewCloseInput,
  PreviewError,
  PreviewGatewayError,
  PreviewGatewayOpenInput,
  PreviewGatewayOpenResult,
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
  PreviewAutomationError,
  PreviewAutomationHost,
  PreviewAutomationHostFocus,
  PreviewAutomationResponse,
  PreviewAutomationStreamEvent,
} from "./previewAutomation.ts";
import {
  ServerConfigStreamEvent,
  ServerConfig,
  ServerProviderCapabilities,
  ServerProviderCapabilitiesInput,
  ServerProviderCapabilitiesError,
  ServerProviderUpdateError,
  ServerProviderUpdateInput,
  ServerLifecycleStreamEvent,
  ServerRemoveKeybindingInput,
  ServerRemoveKeybindingResult,
  ServerProviderUpdatedPayload,
  ServerTraceDiagnosticsResult,
  ServerProcessDiagnosticsResult,
  ServerProcessResourceHistoryInput,
  ServerProcessResourceHistoryResult,
  ServerSignalProcessInput,
  ServerSignalProcessResult,
  ServerUpsertKeybindingInput,
  ServerUpsertKeybindingResult,
} from "./server.ts";
import {
  ConsumeCodexRateLimitResetInput,
  ConsumeCodexRateLimitResetResult,
  ServerProviderUsageRefreshInput,
  ServerProviderUsageResetError,
  ServerProviderUsageResult,
} from "./providerUsage.ts";
import {
  ServerSettings,
  ServerSettingsError,
  ServerSettingsPatch,
  WorktreeWorkspaceError,
} from "./settings.ts";
import {
  SourceControlCloneRepositoryInput,
  SourceControlCloneRepositoryResult,
  SourceControlDiscoveryInput,
  SourceControlDiscoveryResult,
  SourceControlPublishRepositoryInput,
  SourceControlPublishRepositoryResult,
  SourceControlRepositoryError,
  SourceControlRepositoryInfo,
  SourceControlRepositoryLookupInput,
} from "./sourceControl.ts";
import {
  ProfilerClientBatch,
  ProfilerClientBatchInvalid,
  ProfilerClientBatchResult,
  ProfilerClientBatchTooLarge,
  ProfilerStatus,
} from "./profiler.ts";
import { VcsError, VcsStatusSummary } from "./vcs.ts";
import {
  ProjectWorktreeDiscoveryPolicy,
  VcsWorktreeCatalogSnapshot,
  WorktreeCatalogError,
  WorktreeCatalogInput,
  WorktreeCatalogRefreshInput,
  WorktreeDiscoveryPolicyUpdateInput,
  WorktreeAdoptResult,
  WorktreeAdoptionError,
  WorktreeKey,
  WorktreeOperationError,
  WorktreeRemovalError,
  WorktreeRemovalMode,
  WorktreeRemovalPlan,
  WorktreeRemovalPlanToken,
  WorktreeRemovalResult,
  WorkspaceIdentityError,
  WorkspaceUnavailableError,
} from "./worktree.ts";

export const WorktreeAdoptInput = Schema.Struct({
  commandId: CommandId,
  projectId: ProjectId,
  worktreeKey: WorktreeKey,
  expectedGeneration: NonNegativeInt,
  threadDefaults: Schema.Struct({
    modelSelection: ModelSelection,
    runtimeMode: RuntimeMode,
    interactionMode: ProviderInteractionMode,
  }),
});
export type WorktreeAdoptInput = typeof WorktreeAdoptInput.Type;

const WorktreeThreadDefaults = Schema.Struct({
  modelSelection: ModelSelection,
  runtimeMode: RuntimeMode,
  interactionMode: ProviderInteractionMode,
});

export const WorktreeCreateManagedInput = Schema.Struct({
  commandId: CommandId,
  projectId: ProjectId,
  threadId: ThreadId,
  title: TrimmedNonEmptyString,
  refName: TrimmedNonEmptyString,
  newRefName: Schema.optional(Schema.NullOr(TrimmedNonEmptyString)),
  baseRefName: Schema.optional(Schema.NullOr(TrimmedNonEmptyString)),
  threadDefaults: WorktreeThreadDefaults,
});
export type WorktreeCreateManagedInput = typeof WorktreeCreateManagedInput.Type;

export const WorktreeCreatePanelInput = Schema.Struct({
  commandId: CommandId,
  hostThreadId: ThreadId,
  threadId: ThreadId,
  title: TrimmedNonEmptyString,
  threadDefaults: WorktreeThreadDefaults,
});
export type WorktreeCreatePanelInput = typeof WorktreeCreatePanelInput.Type;

export const WorktreeRetargetInput = Schema.Struct({
  commandId: CommandId,
  projectId: ProjectId,
  threadId: ThreadId,
  worktreeKey: WorktreeKey,
  expectedGeneration: NonNegativeInt,
});
export type WorktreeRetargetInput = typeof WorktreeRetargetInput.Type;

export const WorktreeThreadResult = Schema.Struct({
  threadId: ThreadId,
});
export type WorktreeThreadResult = typeof WorktreeThreadResult.Type;

export const WorktreeManagedCreateResult = Schema.Struct({
  threadId: ThreadId,
  path: TrimmedNonEmptyString,
  refName: TrimmedNonEmptyString,
});
export type WorktreeManagedCreateResult = typeof WorktreeManagedCreateResult.Type;

export const WorktreeGetRemovalPlanInput = Schema.Struct({
  projectId: ProjectId,
  threadId: ThreadId,
});
export type WorktreeGetRemovalPlanInput = typeof WorktreeGetRemovalPlanInput.Type;

export const WorktreeRemoveFromBibCodeInput = Schema.Struct({
  commandId: CommandId,
  projectId: ProjectId,
  threadId: ThreadId,
});
export type WorktreeRemoveFromBibCodeInput = typeof WorktreeRemoveFromBibCodeInput.Type;

export const WorktreeRemoveInput = Schema.Struct({
  commandId: CommandId,
  projectId: ProjectId,
  threadId: ThreadId,
  mode: WorktreeRemovalMode,
  expectedGeneration: NonNegativeInt,
  planToken: WorktreeRemovalPlanToken,
  forceDirty: Schema.Boolean,
  confirmRepositoryWidePrune: Schema.Boolean,
});
export type WorktreeRemoveInput = typeof WorktreeRemoveInput.Type;

export const WS_METHODS = {
  uploadsBegin: "uploads.begin",
  uploadsAppend: "uploads.append",
  uploadsGet: "uploads.get",
  uploadsCancel: "uploads.cancel",
  // Authentication methods
  authConfirmPairing: "auth.confirmPairing",

  // Project registry methods
  projectsList: "projects.list",
  projectsAdd: "projects.add",
  projectsRemove: "projects.remove",
  projectsListEntries: "projects.listEntries",
  projectsSubscribeEntries: "subscribeProjectEntries",
  projectsReadFile: "projects.readFile",
  projectsSearchEntries: "projects.searchEntries",
  projectsWriteFile: "projects.writeFile",
  projectsCreateEntry: "projects.createEntry",
  projectsRenameEntry: "projects.renameEntry",
  projectsDeleteEntry: "projects.deleteEntry",
  projectsDuplicateEntry: "projects.duplicateEntry",

  // CLI session import methods
  agentSessionsScan: "agentSessions.scan",
  agentSessionsImport: "agentSessions.import",

  // Shell methods
  shellOpenInEditor: "shell.openInEditor",

  // Filesystem methods
  filesystemBrowse: "filesystem.browse",
  assetsCreateUrl: "assets.createUrl",
  projectsCreateDownloadUrl: "projects.createDownloadUrl",
  projectsCreateUploadUrl: "projects.createUploadUrl",

  // VCS methods
  vcsPull: "vcs.pull",
  vcsRefreshStatus: "vcs.refreshStatus",
  vcsListRefs: "vcs.listRefs",
  vcsListCommits: "vcs.listCommits",
  vcsClone: "vcs.clone",
  vcsCancelClone: "vcs.cancelClone",
  vcsCreateRef: "vcs.createRef",
  vcsSwitchRef: "vcs.switchRef",
  vcsInit: "vcs.init",
  vcsStageFiles: "vcs.stageFiles",
  vcsUnstageFiles: "vcs.unstageFiles",
  vcsDiscardFiles: "vcs.discardFiles",
  vcsGenerateCommitMessage: "vcs.generateCommitMessage",
  vcsRefreshWorktreeCatalog: "vcs.refreshWorktreeCatalog",
  worktreeUpdateDiscoveryPolicy: "worktree.updateDiscoveryPolicy",
  worktreeAdopt: "worktree.adopt",
  worktreeCreateManaged: "worktree.createManaged",
  worktreeCreatePanel: "worktree.createPanel",
  worktreeRetarget: "worktree.retarget",
  worktreeGetRemovalPlan: "worktree.getRemovalPlan",
  worktreeRemoveFromBibCode: "worktree.removeFromBibCode",
  worktreeRemove: "worktree.remove",

  // Git Manager methods
  gitManagerGetRefs: "gitManager.getRefs",
  gitManagerGetCommits: "gitManager.getCommits",
  gitManagerGetDiff: "gitManager.getDiff",
  gitManagerGetStashes: "gitManager.getStashes",
  gitManagerGetRemoteTags: "gitManager.getRemoteTags",
  gitManagerPreviewMerge: "gitManager.previewMerge",
  gitManagerListPullRequests: "gitManager.listPullRequests",
  gitManagerCommit: "gitManager.commit",
  gitManagerUndoCommit: "gitManager.undoCommit",
  gitManagerDiscard: "gitManager.discard",
  gitManagerStagePartial: "gitManager.stagePartial",
  gitManagerUnstagePartial: "gitManager.unstagePartial",
  gitManagerDiscardPartial: "gitManager.discardPartial",
  gitManagerRunOperation: "gitManager.runOperation",

  // Pull Requests methods
  pullRequestsGetContext: "pullRequests.getContext",
  pullRequestsGetVocabulary: "pullRequests.getVocabulary",
  pullRequestsGetCreateDefaults: "pullRequests.getCreateDefaults",
  pullRequestsList: "pullRequests.list",
  pullRequestsGet: "pullRequests.get",
  pullRequestsGetTimeline: "pullRequests.getTimeline",
  pullRequestsGetCommits: "pullRequests.getCommits",
  pullRequestsGetChecks: "pullRequests.getChecks",
  pullRequestsGetFiles: "pullRequests.getFiles",
  pullRequestsRunAction: "pullRequests.runAction",
  pullRequestsCheckout: "pullRequests.checkout",
  pullRequestsReadSnapshot: "pullRequests.readSnapshot",
  pullRequestsSubscribe: "pullRequests.subscribe",

  // Git workflow methods
  gitRunStackedAction: "git.runStackedAction",
  gitResolvePullRequest: "git.resolvePullRequest",
  gitPreparePullRequestThread: "git.preparePullRequestThread",

  // Review methods
  reviewGetDiffPreview: "review.getDiffPreview",

  // Activity methods
  activityGetSnapshot: "activity.getSnapshot",
  activityListRoster: "activity.listRoster",
  activityListDetail: "activity.listDetail",
  activityCancelSubtree: "activity.cancelSubtree",
  activityRetrySubtreeCancellation: "activity.retrySubtreeCancellation",

  // Terminal methods
  terminalOpen: "terminal.open",
  terminalAttach: "terminal.attach",
  terminalWrite: "terminal.write",
  terminalBeginInput: "terminal.beginInput",
  terminalWriteInput: "terminal.writeInput",
  terminalCancelInput: "terminal.cancelInput",
  terminalResize: "terminal.resize",
  terminalClear: "terminal.clear",
  terminalRestart: "terminal.restart",
  terminalClose: "terminal.close",
  terminalStageImagePaste: "terminal.stageImagePaste",

  // Preview methods
  previewOpen: "preview.open",
  previewNavigate: "preview.navigate",
  previewResize: "preview.resize",
  previewRefresh: "preview.refresh",
  previewClose: "preview.close",
  previewList: "preview.list",
  previewReportStatus: "preview.reportStatus",
  previewClaimOpenRequest: "preview.claimOpenRequest",
  previewGatewayOpen: "preview.gatewayOpen",
  previewAutomationConnect: "previewAutomation.connect",
  previewAutomationRespond: "previewAutomation.respond",
  previewAutomationFocusHost: "previewAutomation.focusHost",

  // Device preview methods
  deviceSubscribeCatalog: "device.subscribeCatalog",
  deviceOpen: "device.open",
  deviceClose: "device.close",
  deviceInput: "device.input",
  deviceRotate: "device.rotate",
  deviceSubscribeFrames: "device.subscribeFrames",

  // Server meta
  serverGetConfig: "server.getConfig",
  serverGetProviderCapabilities: "server.getProviderCapabilities",
  serverRefreshProviders: "server.refreshProviders",
  serverUpdateProvider: "server.updateProvider",
  serverUpsertKeybinding: "server.upsertKeybinding",
  serverRemoveKeybinding: "server.removeKeybinding",
  serverGetSettings: "server.getSettings",
  serverUpdateSettings: "server.updateSettings",
  serverDiscoverSourceControl: "server.discoverSourceControl",
  serverGetTraceDiagnostics: "server.getTraceDiagnostics",
  serverGetProcessDiagnostics: "server.getProcessDiagnostics",
  serverGetProcessResourceHistory: "server.getProcessResourceHistory",
  serverGetProfilerStatus: "server.getProfilerStatus",
  serverReportProfilerClientBatch: "server.reportProfilerClientBatch",
  serverSignalProcess: "server.signalProcess",
  serverGetProviderUsage: "server.getProviderUsage",
  serverRefreshProviderUsage: "server.refreshProviderUsage",
  serverConsumeCodexRateLimitReset: "server.consumeCodexRateLimitReset",

  // Remote updater methods
  updaterActiveWork: "updater.activeWork",
  updaterStatus: "updater.status",
  updaterCheck: "updater.check",
  updaterInstall: "updater.install",

  // Cloud environment methods
  cloudGetRelayClientStatus: "cloud.getRelayClientStatus",
  cloudInstallRelayClient: "cloud.installRelayClient",

  // Source control methods
  sourceControlLookupRepository: "sourceControl.lookupRepository",
  sourceControlCloneRepository: "sourceControl.cloneRepository",
  sourceControlPublishRepository: "sourceControl.publishRepository",

  // Streaming subscriptions
  subscribeVcsStatus: "subscribeVcsStatus",
  subscribeVcsStatusSummary: "subscribeVcsStatusSummary",
  subscribeTerminalEvents: "subscribeTerminalEvents",
  subscribeTerminalMetadata: "subscribeTerminalMetadata",
  subscribePreviewEvents: "subscribePreviewEvents",
  subscribeDiscoveredLocalServers: "subscribeDiscoveredLocalServers",
  subscribeServerConfig: "subscribeServerConfig",
  subscribeServerLifecycle: "subscribeServerLifecycle",
  subscribeAuthAccess: "subscribeAuthAccess",
  subscribeActivity: "subscribeActivity",
  subscribeWorktreeCatalog: "subscribeWorktreeCatalog",
  subscribeGitManagerSignal: "subscribeGitManagerSignal",
} as const;

const AuthConfirmPairingEmpty = Schema.Record(Schema.String, Schema.Never);

export const WsAuthConfirmPairingRpc = Rpc.make(WS_METHODS.authConfirmPairing, {
  payload: AuthConfirmPairingEmpty,
  success: AuthConfirmPairingEmpty,
  error: EnvironmentAuthorizationError,
});

export const WsAgentSessionsScanRpc = Rpc.make(WS_METHODS.agentSessionsScan, {
  payload: AgentSessionScanInput,
  success: AgentSessionScanResult,
  error: Schema.Union([AgentSessionsError, EnvironmentRpcError]),
});

export const WsAgentSessionsImportRpc = Rpc.make(WS_METHODS.agentSessionsImport, {
  payload: AgentSessionImportInput,
  success: AgentSessionImportResult,
  error: Schema.Union([AgentSessionsError, EnvironmentRpcError]),
});

export const WsServerUpsertKeybindingRpc = Rpc.make(WS_METHODS.serverUpsertKeybinding, {
  payload: ServerUpsertKeybindingInput,
  success: ServerUpsertKeybindingResult,
  error: Schema.Union([KeybindingsConfigError, EnvironmentRpcError]),
});

export const WsServerRemoveKeybindingRpc = Rpc.make(WS_METHODS.serverRemoveKeybinding, {
  payload: ServerRemoveKeybindingInput,
  success: ServerRemoveKeybindingResult,
  error: Schema.Union([KeybindingsConfigError, EnvironmentRpcError]),
});

export const WsServerGetConfigRpc = Rpc.make(WS_METHODS.serverGetConfig, {
  payload: Schema.Struct({}),
  success: ServerConfig,
  error: Schema.Union([KeybindingsConfigError, ServerSettingsError, EnvironmentRpcError]),
});

export const WsServerGetProviderCapabilitiesRpc = Rpc.make(
  WS_METHODS.serverGetProviderCapabilities,
  {
    payload: ServerProviderCapabilitiesInput,
    success: ServerProviderCapabilities,
    error: Schema.Union([ServerProviderCapabilitiesError, EnvironmentRpcError]),
  },
);

export const WsServerRefreshProvidersRpc = Rpc.make(WS_METHODS.serverRefreshProviders, {
  payload: Schema.Struct({
    /**
     * When supplied, only refresh this specific provider instance. When
     * omitted, refresh all configured instances — the legacy `refresh()`
     * behaviour retained for transports that still dispatch untargeted
     * refreshes.
     */
    instanceId: Schema.optional(ProviderInstanceId),
  }),
  success: ServerProviderUpdatedPayload,
  error: EnvironmentRpcError,
});

export const WsServerUpdateProviderRpc = Rpc.make(WS_METHODS.serverUpdateProvider, {
  payload: ServerProviderUpdateInput,
  success: ServerProviderUpdatedPayload,
  error: Schema.Union([ServerProviderUpdateError, EnvironmentRpcError]),
});

export const WsServerGetSettingsRpc = Rpc.make(WS_METHODS.serverGetSettings, {
  payload: Schema.Struct({}),
  success: ServerSettings,
  error: Schema.Union([ServerSettingsError, EnvironmentRpcError]),
});

export const WsServerUpdateSettingsRpc = Rpc.make(WS_METHODS.serverUpdateSettings, {
  payload: Schema.Struct({ patch: ServerSettingsPatch }),
  success: ServerSettings,
  error: Schema.Union([ServerSettingsError, WorktreeWorkspaceError, EnvironmentRpcError]),
});

export const WsServerDiscoverSourceControlRpc = Rpc.make(WS_METHODS.serverDiscoverSourceControl, {
  payload: SourceControlDiscoveryInput,
  success: SourceControlDiscoveryResult,
  error: EnvironmentRpcError,
});

export const WsServerGetTraceDiagnosticsRpc = Rpc.make(WS_METHODS.serverGetTraceDiagnostics, {
  payload: Schema.Struct({}),
  success: ServerTraceDiagnosticsResult,
  error: EnvironmentRpcError,
});

export const WsServerGetProcessDiagnosticsRpc = Rpc.make(WS_METHODS.serverGetProcessDiagnostics, {
  payload: Schema.Struct({}),
  success: ServerProcessDiagnosticsResult,
  error: EnvironmentRpcError,
});

export const WsServerGetProcessResourceHistoryRpc = Rpc.make(
  WS_METHODS.serverGetProcessResourceHistory,
  {
    payload: ServerProcessResourceHistoryInput,
    success: ServerProcessResourceHistoryResult,
    error: EnvironmentRpcError,
  },
);

export const WsServerGetProfilerStatusRpc = Rpc.make(WS_METHODS.serverGetProfilerStatus, {
  payload: Schema.Struct({}),
  success: ProfilerStatus,
  error: EnvironmentRpcError,
});

export const WsServerReportProfilerClientBatchRpc = Rpc.make(
  WS_METHODS.serverReportProfilerClientBatch,
  {
    payload: ProfilerClientBatch,
    success: ProfilerClientBatchResult,
    error: Schema.Union([
      ProfilerClientBatchTooLarge,
      ProfilerClientBatchInvalid,
      EnvironmentRpcError,
    ]),
  },
);

export const WsServerSignalProcessRpc = Rpc.make(WS_METHODS.serverSignalProcess, {
  payload: ServerSignalProcessInput,
  success: ServerSignalProcessResult,
  error: EnvironmentRpcError,
});

export const WsServerGetProviderUsageRpc = Rpc.make(WS_METHODS.serverGetProviderUsage, {
  payload: Schema.Struct({}),
  success: ServerProviderUsageResult,
  error: EnvironmentRpcError,
});

export const WsServerRefreshProviderUsageRpc = Rpc.make(WS_METHODS.serverRefreshProviderUsage, {
  payload: ServerProviderUsageRefreshInput,
  success: ServerProviderUsageResult,
  error: EnvironmentRpcError,
});

export const WsServerConsumeCodexRateLimitResetRpc = Rpc.make(
  WS_METHODS.serverConsumeCodexRateLimitReset,
  {
    payload: ConsumeCodexRateLimitResetInput,
    success: ConsumeCodexRateLimitResetResult,
    error: Schema.Union([ServerProviderUsageResetError, EnvironmentRpcError]),
  },
);

export const WsUpdaterActiveWorkRpc = Rpc.make(WS_METHODS.updaterActiveWork, {
  payload: Schema.Struct({}),
  success: RemoteUpdateActiveWork,
  error: Schema.Union([RemoteUpdateActiveWorkError, EnvironmentRpcError]),
});

export const WsUpdaterStatusRpc = Rpc.make(WS_METHODS.updaterStatus, {
  payload: Schema.Struct({}),
  success: RemoteUpdateSnapshot,
  error: EnvironmentRpcError,
});

export const WsUpdaterCheckRpc = Rpc.make(WS_METHODS.updaterCheck, {
  payload: Schema.Struct({}),
  success: RemoteUpdateSnapshot,
  error: EnvironmentRpcError,
});

export const WsUpdaterInstallRpc = Rpc.make(WS_METHODS.updaterInstall, {
  payload: Schema.Struct({}),
  success: RemoteUpdateSnapshot,
  error: Schema.Union([RemoteUpdateInstallError, EnvironmentRpcError]),
});

export const WsCloudGetRelayClientStatusRpc = Rpc.make(WS_METHODS.cloudGetRelayClientStatus, {
  payload: Schema.Struct({}),
  success: RelayClientStatusSchema,
  error: EnvironmentRpcError,
});

export const WsCloudInstallRelayClientRpc = Rpc.make(WS_METHODS.cloudInstallRelayClient, {
  payload: Schema.Struct({}),
  success: RelayClientInstallProgressEventSchema,
  error: Schema.Union([RelayClientInstallFailedError, EnvironmentRpcError]),
  stream: true,
});

export const WsSourceControlLookupRepositoryRpc = Rpc.make(
  WS_METHODS.sourceControlLookupRepository,
  {
    payload: SourceControlRepositoryLookupInput,
    success: SourceControlRepositoryInfo,
    error: Schema.Union([
      SourceControlRepositoryError,
      WorkspaceUnavailableError,
      WorkspaceIdentityError,
      EnvironmentRpcError,
    ]),
  },
);

export const WsSourceControlCloneRepositoryRpc = Rpc.make(WS_METHODS.sourceControlCloneRepository, {
  payload: SourceControlCloneRepositoryInput,
  success: SourceControlCloneRepositoryResult,
  error: Schema.Union([SourceControlRepositoryError, EnvironmentRpcError]),
});

export const WsSourceControlPublishRepositoryRpc = Rpc.make(
  WS_METHODS.sourceControlPublishRepository,
  {
    payload: SourceControlPublishRepositoryInput,
    success: SourceControlPublishRepositoryResult,
    error: Schema.Union([
      SourceControlRepositoryError,
      WorkspaceUnavailableError,
      WorkspaceIdentityError,
      EnvironmentRpcError,
    ]),
  },
);

export const WsProjectsSearchEntriesRpc = Rpc.make(WS_METHODS.projectsSearchEntries, {
  payload: ProjectSearchEntriesInput,
  success: ProjectSearchEntriesResult,
  error: Schema.Union([
    ProjectSearchEntriesError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsListEntriesRpc = Rpc.make(WS_METHODS.projectsListEntries, {
  payload: ProjectListEntriesInput,
  success: ProjectListEntriesResult,
  error: Schema.Union([
    ProjectListEntriesError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsSubscribeEntriesRpc = Rpc.make(WS_METHODS.projectsSubscribeEntries, {
  payload: ProjectSubscribeEntriesInput,
  success: ProjectEntriesChangedEvent,
  error: Schema.Union([
    ProjectListEntriesError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
  stream: true,
});

export const WsProjectsReadFileRpc = Rpc.make(WS_METHODS.projectsReadFile, {
  payload: ProjectReadFileInput,
  success: ProjectReadFileResult,
  error: Schema.Union([
    ProjectReadFileError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsWriteFileRpc = Rpc.make(WS_METHODS.projectsWriteFile, {
  payload: ProjectWriteFileInput,
  success: ProjectWriteFileResult,
  error: Schema.Union([
    ProjectWriteFileError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsCreateEntryRpc = Rpc.make(WS_METHODS.projectsCreateEntry, {
  payload: ProjectCreateEntryInput,
  success: ProjectCreateEntryResult,
  error: Schema.Union([
    ProjectCreateEntryError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsRenameEntryRpc = Rpc.make(WS_METHODS.projectsRenameEntry, {
  payload: ProjectRenameEntryInput,
  success: ProjectRenameEntryResult,
  error: Schema.Union([
    ProjectRenameEntryError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsDeleteEntryRpc = Rpc.make(WS_METHODS.projectsDeleteEntry, {
  payload: ProjectDeleteEntryInput,
  success: ProjectDeleteEntryResult,
  error: Schema.Union([
    ProjectDeleteEntryError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsDuplicateEntryRpc = Rpc.make(WS_METHODS.projectsDuplicateEntry, {
  payload: ProjectDuplicateEntryInput,
  success: ProjectDuplicateEntryResult,
  error: Schema.Union([
    ProjectDuplicateEntryError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsShellOpenInEditorRpc = Rpc.make(WS_METHODS.shellOpenInEditor, {
  payload: LaunchEditorInput,
  error: Schema.Union([
    ExternalLauncherError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsFilesystemBrowseRpc = Rpc.make(WS_METHODS.filesystemBrowse, {
  payload: FilesystemBrowseInput,
  success: FilesystemBrowseResult,
  error: Schema.Union([
    FilesystemBrowseError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsAssetsCreateUrlRpc = Rpc.make(WS_METHODS.assetsCreateUrl, {
  payload: AssetCreateUrlInput,
  success: AssetCreateUrlResult,
  error: Schema.Union([
    AssetAccessError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsCreateDownloadUrlRpc = Rpc.make(WS_METHODS.projectsCreateDownloadUrl, {
  payload: ProjectCreateDownloadUrlInput,
  success: ProjectCreateDownloadUrlResult,
  error: Schema.Union([
    ProjectTransferError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsProjectsCreateUploadUrlRpc = Rpc.make(WS_METHODS.projectsCreateUploadUrl, {
  payload: ProjectCreateUploadUrlInput,
  success: ProjectCreateUploadUrlResult,
  error: Schema.Union([
    ProjectTransferError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsSubscribeVcsStatusRpc = Rpc.make(WS_METHODS.subscribeVcsStatus, {
  payload: VcsStatusInput,
  success: VcsStatusStreamEvent,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
  stream: true,
});

export const WsSubscribeVcsStatusSummaryRpc = Rpc.make(WS_METHODS.subscribeVcsStatusSummary, {
  payload: VcsStatusInput,
  success: VcsStatusSummary,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
  stream: true,
});

export const WsVcsPullRpc = Rpc.make(WS_METHODS.vcsPull, {
  payload: VcsPullInput,
  success: VcsPullResult,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsRefreshStatusRpc = Rpc.make(WS_METHODS.vcsRefreshStatus, {
  payload: VcsStatusInput,
  success: VcsStatusResult,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsGitRunStackedActionRpc = Rpc.make(WS_METHODS.gitRunStackedAction, {
  payload: GitRunStackedActionInput,
  success: GitActionProgressEvent,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
  stream: true,
});

export const WsGitResolvePullRequestRpc = Rpc.make(WS_METHODS.gitResolvePullRequest, {
  payload: GitPullRequestRefInput,
  success: GitResolvePullRequestResult,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsGitPreparePullRequestThreadRpc = Rpc.make(WS_METHODS.gitPreparePullRequestThread, {
  payload: GitPreparePullRequestThreadInput,
  success: GitPreparePullRequestThreadResult,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsListRefsRpc = Rpc.make(WS_METHODS.vcsListRefs, {
  payload: VcsListRefsInput,
  success: VcsListRefsResult,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsListCommitsRpc = Rpc.make(WS_METHODS.vcsListCommits, {
  payload: VcsListCommitsInput,
  success: VcsListCommitsResult,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsCloneRpc = Rpc.make(WS_METHODS.vcsClone, {
  payload: GitCloneInput,
  success: GitCloneResult,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
    GitCloneOperationError,
  ]),
});

export const WsVcsCancelCloneRpc = Rpc.make(WS_METHODS.vcsCancelClone, {
  payload: GitCancelCloneInput,
  success: GitCancelCloneResult,
  error: Schema.Union([GitCommandError, EnvironmentRpcError]),
});

export const WsVcsCreateRefRpc = Rpc.make(WS_METHODS.vcsCreateRef, {
  payload: VcsCreateRefInput,
  success: VcsCreateRefResult,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsSwitchRefRpc = Rpc.make(WS_METHODS.vcsSwitchRef, {
  payload: VcsSwitchRefInput,
  success: VcsSwitchRefResult,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsInitRpc = Rpc.make(WS_METHODS.vcsInit, {
  payload: VcsInitInput,
  error: Schema.Union([
    VcsError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsStageFilesRpc = Rpc.make(WS_METHODS.vcsStageFiles, {
  payload: VcsStageFilesInput,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsUnstageFilesRpc = Rpc.make(WS_METHODS.vcsUnstageFiles, {
  payload: VcsUnstageFilesInput,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsDiscardFilesRpc = Rpc.make(WS_METHODS.vcsDiscardFiles, {
  payload: VcsDiscardFilesInput,
  error: Schema.Union([
    GitCommandError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsVcsGenerateCommitMessageRpc = Rpc.make(WS_METHODS.vcsGenerateCommitMessage, {
  payload: VcsGenerateCommitMessageInput,
  success: VcsGenerateCommitMessageResult,
  error: Schema.Union([
    GitManagerServiceError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsSubscribeWorktreeCatalogRpc = Rpc.make(WS_METHODS.subscribeWorktreeCatalog, {
  payload: WorktreeCatalogInput,
  success: VcsWorktreeCatalogSnapshot,
  error: Schema.Union([WorktreeCatalogError, EnvironmentRpcError]),
  stream: true,
});

export const WsVcsRefreshWorktreeCatalogRpc = Rpc.make(WS_METHODS.vcsRefreshWorktreeCatalog, {
  payload: WorktreeCatalogRefreshInput,
  success: VcsWorktreeCatalogSnapshot,
  error: Schema.Union([WorktreeCatalogError, EnvironmentRpcError]),
});

export const WsWorktreeUpdateDiscoveryPolicyRpc = Rpc.make(
  WS_METHODS.worktreeUpdateDiscoveryPolicy,
  {
    payload: WorktreeDiscoveryPolicyUpdateInput,
    success: ProjectWorktreeDiscoveryPolicy,
    error: Schema.Union([WorktreeCatalogError, WorktreeOperationError, EnvironmentRpcError]),
  },
);

export const WsWorktreeAdoptRpc = Rpc.make(WS_METHODS.worktreeAdopt, {
  payload: WorktreeAdoptInput,
  success: WorktreeAdoptResult,
  error: Schema.Union([WorktreeAdoptionError, WorktreeOperationError, EnvironmentRpcError]),
});

export const WsWorktreeCreateManagedRpc = Rpc.make(WS_METHODS.worktreeCreateManaged, {
  payload: WorktreeCreateManagedInput,
  success: WorktreeManagedCreateResult,
  error: Schema.Union([
    WorktreeAdoptionError,
    WorktreeOperationError,
    GitCommandError,
    EnvironmentRpcError,
  ]),
});

export const WsWorktreeCreatePanelRpc = Rpc.make(WS_METHODS.worktreeCreatePanel, {
  payload: WorktreeCreatePanelInput,
  success: WorktreeThreadResult,
  error: Schema.Union([WorktreeAdoptionError, WorktreeOperationError, EnvironmentRpcError]),
});

export const WsWorktreeRetargetRpc = Rpc.make(WS_METHODS.worktreeRetarget, {
  payload: WorktreeRetargetInput,
  success: WorktreeThreadResult,
  error: Schema.Union([WorktreeAdoptionError, WorktreeOperationError, EnvironmentRpcError]),
});

export const WsWorktreeGetRemovalPlanRpc = Rpc.make(WS_METHODS.worktreeGetRemovalPlan, {
  payload: WorktreeGetRemovalPlanInput,
  success: WorktreeRemovalPlan,
  error: Schema.Union([WorktreeRemovalError, WorktreeOperationError, EnvironmentRpcError]),
});

export const WsWorktreeRemoveFromBibCodeRpc = Rpc.make(WS_METHODS.worktreeRemoveFromBibCode, {
  payload: WorktreeRemoveFromBibCodeInput,
  success: WorktreeRemovalResult,
  error: Schema.Union([
    WorktreeRemovalError,
    WorktreeOperationError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsWorktreeRemoveRpc = Rpc.make(WS_METHODS.worktreeRemove, {
  payload: WorktreeRemoveInput,
  success: WorktreeRemovalResult,
  error: Schema.Union([
    WorktreeRemovalError,
    WorktreeOperationError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsGitManagerGetRefsRpc = Rpc.make(WS_METHODS.gitManagerGetRefs, {
  payload: GitManagerCwdInput,
  success: GitManagerRefsSnapshot,
  error: GitManagerOperationError,
});

export const WsGitManagerGetCommitsRpc = Rpc.make(WS_METHODS.gitManagerGetCommits, {
  payload: GitManagerGetCommitsInput,
  success: GitManagerCommitPage,
  error: GitManagerOperationError,
});

export const WsGitManagerGetDiffRpc = Rpc.make(WS_METHODS.gitManagerGetDiff, {
  payload: GitManagerGetDiffInput,
  success: GitManagerDiff,
  error: GitManagerOperationError,
});

export const WsGitManagerGetStashesRpc = Rpc.make(WS_METHODS.gitManagerGetStashes, {
  payload: GitManagerCwdInput,
  success: Schema.Array(GitManagerStashEntry),
  error: GitManagerOperationError,
});

export const WsGitManagerGetRemoteTagsRpc = Rpc.make(WS_METHODS.gitManagerGetRemoteTags, {
  payload: GitManagerRemoteTagsInput,
  success: GitManagerRemoteTags,
  error: GitManagerOperationError,
});

export const WsGitManagerPreviewMergeRpc = Rpc.make(WS_METHODS.gitManagerPreviewMerge, {
  payload: GitManagerPreviewMergeInput,
  success: GitManagerMergePreview,
  error: GitManagerOperationError,
});

export const WsGitManagerListPullRequestsRpc = Rpc.make(WS_METHODS.gitManagerListPullRequests, {
  payload: GitManagerCwdInput,
  success: GitManagerPullRequestsResult,
  error: GitManagerOperationError,
});

export const WsGitManagerCommitRpc = Rpc.make(WS_METHODS.gitManagerCommit, {
  payload: GitManagerCommitInput,
  success: GitManagerCommitResult,
  error: GitManagerOperationError,
});

export const WsGitManagerUndoCommitRpc = Rpc.make(WS_METHODS.gitManagerUndoCommit, {
  payload: GitManagerCwdInput,
  success: GitManagerUndoCommitResult,
  error: GitManagerOperationError,
});

export const WsGitManagerDiscardRpc = Rpc.make(WS_METHODS.gitManagerDiscard, {
  payload: GitManagerDiscardInput,
  success: GitManagerDiscardResult,
  error: GitManagerOperationError,
});

export const WsGitManagerStagePartialRpc = Rpc.make(WS_METHODS.gitManagerStagePartial, {
  payload: GitManagerPartialSelectionInput,
  success: GitManagerPartialSelectionResult,
  error: GitManagerOperationError,
});

export const WsGitManagerUnstagePartialRpc = Rpc.make(WS_METHODS.gitManagerUnstagePartial, {
  payload: GitManagerPartialSelectionInput,
  success: GitManagerPartialSelectionResult,
  error: GitManagerOperationError,
});

export const WsGitManagerDiscardPartialRpc = Rpc.make(WS_METHODS.gitManagerDiscardPartial, {
  payload: GitManagerPartialSelectionInput,
  success: GitManagerPartialSelectionResult,
  error: GitManagerOperationError,
});

export const WsGitManagerRunOperationRpc = Rpc.make(WS_METHODS.gitManagerRunOperation, {
  payload: GitManagerOperationRequest,
  success: GitManagerOperationEvent,
  error: GitManagerOperationError,
  stream: true,
});

export const WsSubscribeGitManagerSignalRpc = Rpc.make(WS_METHODS.subscribeGitManagerSignal, {
  payload: GitManagerCwdInput,
  success: GitManagerSignalEvent,
  error: GitManagerOperationError,
  stream: true,
});

export const WsPullRequestsGetContextRpc = Rpc.make(WS_METHODS.pullRequestsGetContext, {
  payload: PullRequestsGetContextInput,
  success: PullRequestsContext,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetVocabularyRpc = Rpc.make(WS_METHODS.pullRequestsGetVocabulary, {
  payload: PullRequestsVocabularyInput,
  success: PullRequestsVocabulary,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetCreateDefaultsRpc = Rpc.make(
  WS_METHODS.pullRequestsGetCreateDefaults,
  {
    payload: PullRequestsCwdInput,
    success: PullRequestsCreateDefaults,
    error: PullRequestsOperationError,
  },
);

export const WsPullRequestsListRpc = Rpc.make(WS_METHODS.pullRequestsList, {
  payload: PullRequestsListInput,
  success: PullRequestsListPage,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetRpc = Rpc.make(WS_METHODS.pullRequestsGet, {
  payload: PullRequestsNumberInput,
  success: PullRequestsDetail,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetTimelineRpc = Rpc.make(WS_METHODS.pullRequestsGetTimeline, {
  payload: PullRequestsNumberInput,
  success: PullRequestsTimeline,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetCommitsRpc = Rpc.make(WS_METHODS.pullRequestsGetCommits, {
  payload: PullRequestsNumberInput,
  success: PullRequestsCommits,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetChecksRpc = Rpc.make(WS_METHODS.pullRequestsGetChecks, {
  payload: PullRequestsNumberInput,
  success: PullRequestsChecks,
  error: PullRequestsOperationError,
});

export const WsPullRequestsGetFilesRpc = Rpc.make(WS_METHODS.pullRequestsGetFiles, {
  payload: PullRequestsNumberInput,
  success: PullRequestsFiles,
  error: PullRequestsOperationError,
});

export const WsPullRequestsRunActionRpc = Rpc.make(WS_METHODS.pullRequestsRunAction, {
  payload: PullRequestsActionRequest,
  success: PullRequestsActionResult,
  error: PullRequestsOperationError,
});

export const WsPullRequestsCheckoutRpc = Rpc.make(WS_METHODS.pullRequestsCheckout, {
  payload: PullRequestsCheckoutInput,
  success: PullRequestsCheckoutResult,
  error: PullRequestsOperationError,
});

export const WsPullRequestsReadSnapshotRpc = Rpc.make(WS_METHODS.pullRequestsReadSnapshot, {
  payload: PullRequestsSubscribeInput,
  success: PullRequestsSnapshot,
  error: PullRequestsOperationError,
});

export const WsPullRequestsSubscribeRpc = Rpc.make(WS_METHODS.pullRequestsSubscribe, {
  payload: PullRequestsSubscribeInput,
  success: PullRequestsChanged,
  error: PullRequestsOperationError,
  stream: true,
});

/**
 * Ephemeral live diff preview for compact/mobile surfaces.
 * Not the persisted BiBCode Review model. Future review sessions should use
 * review.open* + review.getSnapshot.
 */
export const WsReviewGetDiffPreviewRpc = Rpc.make(WS_METHODS.reviewGetDiffPreview, {
  payload: ReviewDiffPreviewInput,
  success: ReviewDiffPreviewResult,
  error: Schema.Union([
    ReviewDiffPreviewError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsTerminalOpenRpc = Rpc.make(WS_METHODS.terminalOpen, {
  payload: TerminalOpenInput,
  success: TerminalSessionSnapshot,
  error: Schema.Union([
    TerminalError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsTerminalAttachRpc = Rpc.make(WS_METHODS.terminalAttach, {
  payload: TerminalAttachInput,
  success: TerminalAttachStreamEvent,
  error: Schema.Union([
    TerminalError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
  stream: true,
});

export const WsTerminalWriteRpc = Rpc.make(WS_METHODS.terminalWrite, {
  payload: TerminalWriteInput,
  error: Schema.Union([
    TerminalError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

const TerminalOrderedInputError = Schema.Union([
  TerminalInputError,
  TerminalError,
  WorkspaceUnavailableError,
  WorkspaceIdentityError,
  EnvironmentRpcError,
]);

export const WsTerminalBeginInputRpc = Rpc.make(WS_METHODS.terminalBeginInput, {
  payload: TerminalBeginInput,
  success: TerminalInputLease,
  error: TerminalOrderedInputError,
});

export const WsTerminalWriteInputRpc = Rpc.make(WS_METHODS.terminalWriteInput, {
  payload: TerminalWriteInputFrame,
  success: TerminalInputAcknowledgement,
  error: TerminalOrderedInputError,
});

export const WsTerminalCancelInputRpc = Rpc.make(WS_METHODS.terminalCancelInput, {
  payload: TerminalCancelInput,
  success: Schema.Null,
  error: TerminalOrderedInputError,
});

export const WsTerminalResizeRpc = Rpc.make(WS_METHODS.terminalResize, {
  payload: TerminalResizeInput,
  error: Schema.Union([TerminalError, EnvironmentRpcError]),
});

export const WsTerminalClearRpc = Rpc.make(WS_METHODS.terminalClear, {
  payload: TerminalClearInput,
  error: Schema.Union([TerminalError, EnvironmentRpcError]),
});

export const WsTerminalRestartRpc = Rpc.make(WS_METHODS.terminalRestart, {
  payload: TerminalRestartInput,
  success: TerminalSessionSnapshot,
  error: Schema.Union([
    TerminalError,
    WorkspaceUnavailableError,
    WorkspaceIdentityError,
    EnvironmentRpcError,
  ]),
});

export const WsTerminalCloseRpc = Rpc.make(WS_METHODS.terminalClose, {
  payload: TerminalCloseInput,
  error: Schema.Union([TerminalError, EnvironmentRpcError]),
});

export const WsPreviewOpenRpc = Rpc.make(WS_METHODS.previewOpen, {
  payload: PreviewOpenInput,
  success: PreviewSessionSnapshot,
  error: Schema.Union([PreviewError, EnvironmentRpcError]),
});

export const WsPreviewNavigateRpc = Rpc.make(WS_METHODS.previewNavigate, {
  payload: PreviewNavigateInput,
  success: PreviewSessionSnapshot,
  error: Schema.Union([PreviewError, EnvironmentRpcError]),
});

export const WsPreviewResizeRpc = Rpc.make(WS_METHODS.previewResize, {
  payload: PreviewResizeInput,
  success: PreviewSessionSnapshot,
  error: Schema.Union([PreviewError, EnvironmentRpcError]),
});

export const WsPreviewRefreshRpc = Rpc.make(WS_METHODS.previewRefresh, {
  payload: PreviewRefreshInput,
  error: Schema.Union([PreviewError, EnvironmentRpcError]),
});

export const WsPreviewCloseRpc = Rpc.make(WS_METHODS.previewClose, {
  payload: PreviewCloseInput,
  error: Schema.Union([PreviewError, EnvironmentRpcError]),
});

export const WsPreviewListRpc = Rpc.make(WS_METHODS.previewList, {
  payload: PreviewListInput,
  success: PreviewListResult,
  error: EnvironmentRpcError,
});

export const WsPreviewReportStatusRpc = Rpc.make(WS_METHODS.previewReportStatus, {
  payload: PreviewReportStatusInput,
  error: Schema.Union([PreviewError, EnvironmentRpcError]),
});

export const WsPreviewClaimOpenRequestRpc = Rpc.make(WS_METHODS.previewClaimOpenRequest, {
  payload: PreviewClaimOpenRequestInput,
  success: PreviewClaimOpenRequestResult,
  error: EnvironmentRpcError,
});

export const WsPreviewGatewayOpenRpc = Rpc.make(WS_METHODS.previewGatewayOpen, {
  payload: PreviewGatewayOpenInput,
  success: PreviewGatewayOpenResult,
  error: Schema.Union([PreviewGatewayError, EnvironmentRpcError]),
});

const DeviceRpcFailure = Schema.Union([DeviceRpcError, EnvironmentRpcError]);

export const WsDeviceSubscribeCatalogRpc = Rpc.make(WS_METHODS.deviceSubscribeCatalog, {
  payload: Schema.Struct({}),
  success: DeviceCatalog,
  error: DeviceRpcFailure,
  stream: true,
});

export const WsDeviceOpenRpc = Rpc.make(WS_METHODS.deviceOpen, {
  payload: DeviceOpenInput,
  success: DeviceOpenResult,
  error: DeviceRpcFailure,
});

export const WsDeviceCloseRpc = Rpc.make(WS_METHODS.deviceClose, {
  payload: DeviceIdInput,
  error: DeviceRpcFailure,
});

export const WsDeviceInputRpc = Rpc.make(WS_METHODS.deviceInput, {
  payload: DeviceInputInput,
  error: DeviceRpcFailure,
});

export const WsDeviceRotateRpc = Rpc.make(WS_METHODS.deviceRotate, {
  payload: DeviceRotateInput,
  error: DeviceRpcFailure,
});

export const WsDeviceSubscribeFramesRpc = Rpc.make(WS_METHODS.deviceSubscribeFrames, {
  payload: DeviceIdInput,
  success: DeviceJpegFrame,
  error: DeviceRpcFailure,
  stream: true,
});

export const WsPreviewAutomationConnectRpc = Rpc.make(WS_METHODS.previewAutomationConnect, {
  payload: PreviewAutomationHost,
  success: PreviewAutomationStreamEvent,
  error: Schema.Union([PreviewAutomationError, EnvironmentRpcError]),
  stream: true,
});

export const WsPreviewAutomationRespondRpc = Rpc.make(WS_METHODS.previewAutomationRespond, {
  payload: PreviewAutomationResponse,
  error: Schema.Union([PreviewAutomationError, EnvironmentRpcError]),
});

export const WsPreviewAutomationFocusHostRpc = Rpc.make(WS_METHODS.previewAutomationFocusHost, {
  payload: PreviewAutomationHostFocus,
  error: EnvironmentRpcError,
});

export const WsSubscribePreviewEventsRpc = Rpc.make(WS_METHODS.subscribePreviewEvents, {
  payload: Schema.Struct({}),
  success: PreviewEvent,
  error: EnvironmentRpcError,
  stream: true,
});

export const WsSubscribeDiscoveredLocalServersRpc = Rpc.make(
  WS_METHODS.subscribeDiscoveredLocalServers,
  {
    payload: Schema.Struct({}),
    success: DiscoveredLocalServerList,
    error: EnvironmentRpcError,
    stream: true,
  },
);

export const WsOrchestrationDispatchCommandRpc = Rpc.make(
  ORCHESTRATION_WS_METHODS.dispatchCommand,
  {
    payload: ClientOrchestrationCommand,
    success: OrchestrationRpcSchemas.dispatchCommand.output,
    error: Schema.Union([
      OrchestrationDispatchCommandError,
      UploadError,
      WorkspaceUnavailableError,
      WorkspaceIdentityError,
      EnvironmentRpcError,
    ]),
  },
);

export const WsOrchestrationGetTurnDiffRpc = Rpc.make(ORCHESTRATION_WS_METHODS.getTurnDiff, {
  payload: OrchestrationGetTurnDiffInput,
  success: OrchestrationRpcSchemas.getTurnDiff.output,
  error: Schema.Union([OrchestrationGetTurnDiffError, EnvironmentRpcError]),
});

export const WsOrchestrationGetFullThreadDiffRpc = Rpc.make(
  ORCHESTRATION_WS_METHODS.getFullThreadDiff,
  {
    payload: OrchestrationGetFullThreadDiffInput,
    success: OrchestrationRpcSchemas.getFullThreadDiff.output,
    error: Schema.Union([OrchestrationGetFullThreadDiffError, EnvironmentRpcError]),
  },
);

export const WsOrchestrationReplayEventsRpc = Rpc.make(ORCHESTRATION_WS_METHODS.replayEvents, {
  payload: OrchestrationReplayEventsInput,
  success: OrchestrationRpcSchemas.replayEvents.output,
  error: Schema.Union([OrchestrationReplayEventsError, EnvironmentRpcError]),
});

export const WsOrchestrationGetArchivedShellSnapshotRpc = Rpc.make(
  ORCHESTRATION_WS_METHODS.getArchivedShellSnapshot,
  {
    payload: OrchestrationRpcSchemas.getArchivedShellSnapshot.input,
    success: OrchestrationRpcSchemas.getArchivedShellSnapshot.output,
    error: Schema.Union([OrchestrationGetSnapshotError, EnvironmentRpcError]),
  },
);

export const WsOrchestrationSubscribeShellRpc = Rpc.make(ORCHESTRATION_WS_METHODS.subscribeShell, {
  payload: OrchestrationRpcSchemas.subscribeShell.input,
  success: OrchestrationRpcSchemas.subscribeShell.output,
  error: Schema.Union([OrchestrationGetSnapshotError, EnvironmentRpcError]),
  stream: true,
});

export const WsOrchestrationSubscribeThreadRpc = Rpc.make(
  ORCHESTRATION_WS_METHODS.subscribeThread,
  {
    payload: OrchestrationRpcSchemas.subscribeThread.input,
    success: OrchestrationRpcSchemas.subscribeThread.output,
    error: Schema.Union([OrchestrationGetSnapshotError, EnvironmentRpcError]),
    stream: true,
  },
);

export const WsSubscribeTerminalEventsRpc = Rpc.make(WS_METHODS.subscribeTerminalEvents, {
  payload: Schema.Struct({}),
  success: TerminalEvent,
  error: EnvironmentRpcError,
  stream: true,
});

export const WsSubscribeTerminalMetadataRpc = Rpc.make(WS_METHODS.subscribeTerminalMetadata, {
  payload: Schema.Struct({}),
  success: TerminalMetadataStreamEvent,
  error: EnvironmentRpcError,
  stream: true,
});

export const WsSubscribeServerConfigRpc = Rpc.make(WS_METHODS.subscribeServerConfig, {
  payload: Schema.Struct({}),
  success: ServerConfigStreamEvent,
  error: Schema.Union([KeybindingsConfigError, ServerSettingsError, EnvironmentRpcError]),
  stream: true,
});

export const WsSubscribeServerLifecycleRpc = Rpc.make(WS_METHODS.subscribeServerLifecycle, {
  payload: Schema.Struct({}),
  success: ServerLifecycleStreamEvent,
  error: EnvironmentRpcError,
  stream: true,
});

export const WsSubscribeAuthAccessRpc = Rpc.make(WS_METHODS.subscribeAuthAccess, {
  payload: Schema.Struct({}),
  success: AuthAccessStreamEvent,
  error: Schema.Union([AuthAccessStreamError, EnvironmentRpcError]),
  stream: true,
});

export const WsActivityGetSnapshotRpc = Rpc.make(WS_METHODS.activityGetSnapshot, {
  payload: ActivityGetSnapshotInput,
  success: ActivitySnapshot,
  error: Schema.Union([ActivityError, EnvironmentRpcError]),
});

export const WsActivityListRosterRpc = Rpc.make(WS_METHODS.activityListRoster, {
  payload: ActivityListRosterInput,
  success: ActivityRosterPage,
  error: Schema.Union([ActivityError, EnvironmentRpcError]),
});

export const WsActivityListDetailRpc = Rpc.make(WS_METHODS.activityListDetail, {
  payload: ActivityListDetailInput,
  success: ActivityDetailPage,
  error: Schema.Union([ActivityError, EnvironmentRpcError]),
});

export const WsActivityCancelSubtreeRpc = Rpc.make(WS_METHODS.activityCancelSubtree, {
  payload: ActivityCancelSubtreeInput,
  success: ActivitySubtreeCancellationResult,
  error: Schema.Union([ActivityError, EnvironmentRpcError]),
});

export const WsActivityRetrySubtreeCancellationRpc = Rpc.make(
  WS_METHODS.activityRetrySubtreeCancellation,
  {
    payload: ActivityRetrySubtreeCancellationInput,
    success: ActivitySubtreeCancellationResult,
    error: Schema.Union([ActivityError, EnvironmentRpcError]),
  },
);

export const WsSubscribeActivityRpc = Rpc.make(WS_METHODS.subscribeActivity, {
  payload: ActivityScopeRef,
  success: ActivityStreamItem,
  error: Schema.Union([ActivityError, EnvironmentRpcError]),
  stream: true,
});

export const WsUploadsBeginRpc = Rpc.make(WS_METHODS.uploadsBegin, {
  payload: UploadBeginInput,
  success: UploadBeginResult,
  error: Schema.Union([UploadError, EnvironmentRpcError]),
});
export const WsUploadsAppendRpc = Rpc.make(WS_METHODS.uploadsAppend, {
  payload: UploadAppendInput,
  success: UploadAppendResult,
  error: Schema.Union([UploadError, EnvironmentRpcError]),
});
export const WsUploadsGetRpc = Rpc.make(WS_METHODS.uploadsGet, {
  payload: UploadGetInput,
  success: UploadGetResult,
  error: Schema.Union([UploadError, EnvironmentRpcError]),
});
export const WsUploadsCancelRpc = Rpc.make(WS_METHODS.uploadsCancel, {
  payload: UploadCancelInput,
  success: UploadCancelResult,
  error: Schema.Union([UploadError, EnvironmentRpcError]),
});
export const WsTerminalStageImagePasteRpc = Rpc.make(WS_METHODS.terminalStageImagePaste, {
  payload: TerminalStageImagePasteInput,
  success: TerminalStageImagePasteResult,
  error: Schema.Union([UploadError, EnvironmentRpcError]),
});

export const WsRpcGroup = RpcGroup.make(
  WsUploadsBeginRpc,
  WsUploadsAppendRpc,
  WsUploadsGetRpc,
  WsUploadsCancelRpc,
  WsAuthConfirmPairingRpc,
  WsAgentSessionsScanRpc,
  WsAgentSessionsImportRpc,
  WsServerGetConfigRpc,
  WsServerGetProviderCapabilitiesRpc,
  WsServerRefreshProvidersRpc,
  WsServerUpdateProviderRpc,
  WsServerUpsertKeybindingRpc,
  WsServerRemoveKeybindingRpc,
  WsServerGetSettingsRpc,
  WsServerUpdateSettingsRpc,
  WsServerDiscoverSourceControlRpc,
  WsServerGetTraceDiagnosticsRpc,
  WsServerGetProcessDiagnosticsRpc,
  WsServerGetProcessResourceHistoryRpc,
  WsServerGetProfilerStatusRpc,
  WsServerReportProfilerClientBatchRpc,
  WsServerSignalProcessRpc,
  WsServerGetProviderUsageRpc,
  WsServerRefreshProviderUsageRpc,
  WsServerConsumeCodexRateLimitResetRpc,
  WsUpdaterActiveWorkRpc,
  WsUpdaterStatusRpc,
  WsUpdaterCheckRpc,
  WsUpdaterInstallRpc,
  WsCloudGetRelayClientStatusRpc,
  WsCloudInstallRelayClientRpc,
  WsSourceControlLookupRepositoryRpc,
  WsSourceControlCloneRepositoryRpc,
  WsSourceControlPublishRepositoryRpc,
  WsProjectsListEntriesRpc,
  WsProjectsSubscribeEntriesRpc,
  WsProjectsReadFileRpc,
  WsProjectsSearchEntriesRpc,
  WsProjectsWriteFileRpc,
  WsProjectsCreateEntryRpc,
  WsProjectsRenameEntryRpc,
  WsProjectsDeleteEntryRpc,
  WsProjectsDuplicateEntryRpc,
  WsShellOpenInEditorRpc,
  WsFilesystemBrowseRpc,
  WsAssetsCreateUrlRpc,
  WsProjectsCreateDownloadUrlRpc,
  WsProjectsCreateUploadUrlRpc,
  WsSubscribeVcsStatusRpc,
  WsSubscribeVcsStatusSummaryRpc,
  WsVcsPullRpc,
  WsVcsRefreshStatusRpc,
  WsGitRunStackedActionRpc,
  WsGitResolvePullRequestRpc,
  WsGitPreparePullRequestThreadRpc,
  WsVcsListRefsRpc,
  WsVcsListCommitsRpc,
  WsVcsCloneRpc,
  WsVcsCancelCloneRpc,
  WsVcsCreateRefRpc,
  WsVcsSwitchRefRpc,
  WsVcsInitRpc,
  WsVcsStageFilesRpc,
  WsVcsUnstageFilesRpc,
  WsVcsDiscardFilesRpc,
  WsVcsGenerateCommitMessageRpc,
  WsSubscribeWorktreeCatalogRpc,
  WsVcsRefreshWorktreeCatalogRpc,
  WsWorktreeUpdateDiscoveryPolicyRpc,
  WsWorktreeAdoptRpc,
  WsWorktreeCreateManagedRpc,
  WsWorktreeCreatePanelRpc,
  WsWorktreeRetargetRpc,
  WsWorktreeGetRemovalPlanRpc,
  WsWorktreeRemoveFromBibCodeRpc,
  WsWorktreeRemoveRpc,
  WsGitManagerGetRefsRpc,
  WsGitManagerGetCommitsRpc,
  WsGitManagerGetDiffRpc,
  WsGitManagerGetStashesRpc,
  WsGitManagerGetRemoteTagsRpc,
  WsGitManagerPreviewMergeRpc,
  WsGitManagerListPullRequestsRpc,
  WsGitManagerCommitRpc,
  WsGitManagerUndoCommitRpc,
  WsGitManagerDiscardRpc,
  WsGitManagerStagePartialRpc,
  WsGitManagerUnstagePartialRpc,
  WsGitManagerDiscardPartialRpc,
  WsGitManagerRunOperationRpc,
  WsSubscribeGitManagerSignalRpc,
  WsPullRequestsGetContextRpc,
  WsPullRequestsGetVocabularyRpc,
  WsPullRequestsGetCreateDefaultsRpc,
  WsPullRequestsListRpc,
  WsPullRequestsGetRpc,
  WsPullRequestsGetTimelineRpc,
  WsPullRequestsGetCommitsRpc,
  WsPullRequestsGetChecksRpc,
  WsPullRequestsGetFilesRpc,
  WsPullRequestsRunActionRpc,
  WsPullRequestsCheckoutRpc,
  WsPullRequestsReadSnapshotRpc,
  WsPullRequestsSubscribeRpc,
  WsReviewGetDiffPreviewRpc,
  WsTerminalOpenRpc,
  WsTerminalAttachRpc,
  WsTerminalWriteRpc,
  WsTerminalBeginInputRpc,
  WsTerminalWriteInputRpc,
  WsTerminalCancelInputRpc,
  WsTerminalResizeRpc,
  WsTerminalClearRpc,
  WsTerminalRestartRpc,
  WsTerminalCloseRpc,
  WsTerminalStageImagePasteRpc,
  WsSubscribeTerminalEventsRpc,
  WsSubscribeTerminalMetadataRpc,
  WsPreviewOpenRpc,
  WsPreviewNavigateRpc,
  WsPreviewResizeRpc,
  WsPreviewRefreshRpc,
  WsPreviewCloseRpc,
  WsPreviewListRpc,
  WsPreviewReportStatusRpc,
  WsPreviewClaimOpenRequestRpc,
  WsPreviewGatewayOpenRpc,
  WsPreviewAutomationConnectRpc,
  WsPreviewAutomationRespondRpc,
  WsPreviewAutomationFocusHostRpc,
  WsDeviceSubscribeCatalogRpc,
  WsDeviceOpenRpc,
  WsDeviceCloseRpc,
  WsDeviceInputRpc,
  WsDeviceRotateRpc,
  WsDeviceSubscribeFramesRpc,
  WsSubscribePreviewEventsRpc,
  WsSubscribeDiscoveredLocalServersRpc,
  WsSubscribeServerConfigRpc,
  WsSubscribeServerLifecycleRpc,
  WsSubscribeAuthAccessRpc,
  WsActivityGetSnapshotRpc,
  WsActivityListRosterRpc,
  WsActivityListDetailRpc,
  WsActivityCancelSubtreeRpc,
  WsActivityRetrySubtreeCancellationRpc,
  WsSubscribeActivityRpc,
  WsOrchestrationDispatchCommandRpc,
  WsOrchestrationGetTurnDiffRpc,
  WsOrchestrationGetFullThreadDiffRpc,
  WsOrchestrationReplayEventsRpc,
  WsOrchestrationGetArchivedShellSnapshotRpc,
  WsOrchestrationSubscribeShellRpc,
  WsOrchestrationSubscribeThreadRpc,
);
