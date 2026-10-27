import {
  createAttachmentAdmissionOwner,
  type AttachmentSource,
  type StartThreadTurnInput,
} from "@coddy/client-runtime/operations";
import type { StagedUploadChatAttachment } from "@coddy/contracts";
import type { ComposerThreadDraftState } from "../composerDraftStore";

export interface StagedAttachmentAttempt {
  readonly ambiguousAdmission?: {
    readonly input: StartThreadTurnInput;
    readonly restoredDraft: ComposerThreadDraftState | null;
  };
  readonly threadKey: string;
  readonly createdAt: number;
  readonly sources: ReadonlyArray<AttachmentSource>;
  readonly attachments: ReadonlyArray<StagedUploadChatAttachment>;
}

// Shared by every main/panel ChatView in this application. Routes own no admission identity.
export const attachmentAdmissionOwner = createAttachmentAdmissionOwner<StagedAttachmentAttempt>();
