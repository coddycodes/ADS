import type {
  ChatFileAttachment as ContractChatFileAttachment,
  ChatImageAttachment as ContractChatImageAttachment,
  OrchestrationCheckpointFile,
  OrchestrationCheckpointSummary,
  OrchestrationLatestTurn,
  OrchestrationMessage,
  OrchestrationProposedPlan,
  OrchestrationSession,
  ProjectScript as ContractProjectScript,
  ProviderInteractionMode,
  RuntimeMode,
  TurnDelivery as ContractTurnDelivery,
  TurnDeliveryResolutionAction as ContractTurnDeliveryResolutionAction,
} from "@coddy/contracts";
import type {
  EnvironmentProject,
  EnvironmentThread,
  EnvironmentThreadShell,
} from "@coddy/client-runtime/state/shell";

export type SessionPhase = "disconnected" | "connecting" | "ready" | "running";
export const DEFAULT_RUNTIME_MODE: RuntimeMode = "full-access";

export const DEFAULT_INTERACTION_MODE: ProviderInteractionMode = "default";
export const MAX_TERMINALS_PER_GROUP = 4;
export type ProjectScript = ContractProjectScript;
export type TurnDelivery = ContractTurnDelivery;
export type TurnDeliveryResolutionAction = ContractTurnDeliveryResolutionAction;

export interface ThreadTerminalGroup {
  id: string;
  terminalIds: string[];
  splitDirection?: "horizontal" | "vertical";
}

export interface ChatImageAttachment extends ContractChatImageAttachment {
  readonly previewUrl?: string;
}

export type ChatFileAttachment = ContractChatFileAttachment;

export type ChatAttachment = ChatImageAttachment | ChatFileAttachment;

export interface ChatMessage extends Omit<OrchestrationMessage, "attachments"> {
  readonly attachments?: ReadonlyArray<ChatAttachment> | undefined;
}

export type ProposedPlan = OrchestrationProposedPlan;
export type TurnDiffFileChange = OrchestrationCheckpointFile;
export type TurnDiffSummary = OrchestrationCheckpointSummary;

export type Project = EnvironmentProject;
export type Thread = EnvironmentThread;
export type ThreadShell = EnvironmentThreadShell;

export interface ThreadTurnState {
  latestTurn: OrchestrationLatestTurn | null;
}

export type SidebarThreadSummary = EnvironmentThreadShell;
export type ThreadSession = OrchestrationSession;
