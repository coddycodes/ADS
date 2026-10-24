import * as Crypto from "effect/Crypto";
import type * as Effect from "effect/Effect";
import { Atom } from "effect/reactivity";

import { createAtomCommandScheduler, createEnvironmentCommand } from "./runtime.ts";
import {
  type ArchiveThreadInput,
  type CreateThreadInput,
  type DeleteThreadInput,
  type InterruptThreadTurnInput,
  type RespondToThreadApprovalInput,
  type RespondToThreadUserInputInput,
  type ResolveTurnDeliveryInput,
  type RevertThreadCheckpointInput,
  type SetThreadInteractionModeInput,
  type SetThreadRuntimeModeInput,
  type StartThreadTurnInput,
  type SteerThreadTurnInput,
  type PromoteThreadTurnInput,
  type StopThreadSessionInput,
  type UnarchiveThreadInput,
  type UpdateThreadMetadataInput,
  archiveThread,
  createThread,
  deleteThread,
  interruptThreadTurn,
  respondToThreadApproval,
  respondToThreadUserInput,
  resolveTurnDelivery,
  revertThreadCheckpoint,
  setThreadInteractionMode,
  setThreadRuntimeMode,
  startThreadTurn,
  steerThreadTurn,
  promoteThreadTurn,
  stopThreadSession,
  unarchiveThread,
  updateThreadMetadata,
} from "../operations/commands.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";
import {
  admitStagedThreadTurn,
  readAttachmentAdmissionAuthority,
} from "../operations/attachmentAdmissionAuthority.ts";
import type { AttachmentAdmissionAuthority } from "../operations/attachmentAdmissionOwner.ts";
import {
  stageAttachments,
  releaseStagedAttachments,
  keepStagedAttachmentsAlive,
  type AttachmentStagingInput,
} from "../operations/attachmentStaging.ts";

export type {
  ArchiveThreadInput,
  CreateThreadInput,
  DeleteThreadInput,
  InterruptThreadTurnInput,
  RespondToThreadApprovalInput,
  RespondToThreadUserInputInput,
  ResolveTurnDeliveryInput,
  RevertThreadCheckpointInput,
  SetThreadInteractionModeInput,
  SetThreadRuntimeModeInput,
  StartThreadTurnInput,
  SteerThreadTurnInput,
  PromoteThreadTurnInput,
  StopThreadSessionInput,
  UnarchiveThreadInput,
  UpdateThreadMetadataInput,
} from "../operations/commands.ts";

export function createThreadEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | Crypto.Crypto | R, E>,
) {
  const scheduler = createAtomCommandScheduler();
  const concurrency = {
    mode: "serial" as const,
    key: ({ environmentId, input }: { environmentId: string; input: { threadId: string } }) =>
      JSON.stringify([environmentId, input.threadId]),
  };
  return {
    attachmentAdmissionAuthority: createEnvironmentCommand<
      EnvironmentRegistry | Crypto.Crypto | R,
      E,
      void,
      Effect.Success<ReturnType<typeof readAttachmentAdmissionAuthority>>,
      Effect.Error<ReturnType<typeof readAttachmentAdmissionAuthority>>
    >(runtime, {
      label: "environment-data:commands:thread:attachment-admission-authority",
      execute: () => readAttachmentAdmissionAuthority(),
    }),
    stageAttachments: createEnvironmentCommand<
      EnvironmentRegistry | Crypto.Crypto | R,
      E,
      AttachmentStagingInput,
      Effect.Success<ReturnType<typeof stageAttachments>>,
      Effect.Error<ReturnType<typeof stageAttachments>>
    >(runtime, {
      label: "environment-data:commands:thread:stage-attachments",
      execute: (input: AttachmentStagingInput) => stageAttachments(input),
    }),
    releaseStagedAttachments: createEnvironmentCommand<
      EnvironmentRegistry | Crypto.Crypto | R,
      E,
      Parameters<typeof releaseStagedAttachments>[0],
      Effect.Success<ReturnType<typeof releaseStagedAttachments>>,
      Effect.Error<ReturnType<typeof releaseStagedAttachments>>
    >(runtime, {
      label: "environment-data:commands:thread:release-staged-attachments",
      execute: (input: Parameters<typeof releaseStagedAttachments>[0]) =>
        releaseStagedAttachments(input),
    }),
    keepStagedAttachmentsAlive: createEnvironmentCommand<
      EnvironmentRegistry | Crypto.Crypto | R,
      E,
      Parameters<typeof keepStagedAttachmentsAlive>[0],
      Effect.Success<ReturnType<typeof keepStagedAttachmentsAlive>>,
      Effect.Error<ReturnType<typeof keepStagedAttachmentsAlive>>
    >(runtime, {
      label: "environment-data:commands:thread:keep-staged-attachments-alive",
      execute: (input) => keepStagedAttachmentsAlive(input),
    }),
    create: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:create",
      execute: (input: CreateThreadInput) => createThread(input),
      scheduler,
      concurrency,
    }),
    delete: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:delete",
      execute: (input: DeleteThreadInput) => deleteThread(input),
      scheduler,
      concurrency,
    }),
    archive: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:archive",
      execute: (input: ArchiveThreadInput) => archiveThread(input),
      scheduler,
      concurrency,
    }),
    unarchive: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:unarchive",
      execute: (input: UnarchiveThreadInput) => unarchiveThread(input),
      scheduler,
      concurrency,
    }),
    updateMetadata: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:update-metadata",
      execute: (input: UpdateThreadMetadataInput) => updateThreadMetadata(input),
      scheduler,
      concurrency,
    }),
    setRuntimeMode: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:set-runtime-mode",
      execute: (input: SetThreadRuntimeModeInput) => setThreadRuntimeMode(input),
      scheduler,
      concurrency,
    }),
    setInteractionMode: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:set-interaction-mode",
      execute: (input: SetThreadInteractionModeInput) => setThreadInteractionMode(input),
      scheduler,
      concurrency,
    }),
    startTurn: createEnvironmentCommand<
      EnvironmentRegistry | Crypto.Crypto | R,
      E,
      StartThreadTurnInput & { readonly attachmentAuthority?: AttachmentAdmissionAuthority },
      Effect.Success<ReturnType<typeof startThreadTurn>>,
      | Effect.Error<ReturnType<typeof startThreadTurn>>
      | Effect.Error<ReturnType<typeof admitStagedThreadTurn>>
    >(runtime, {
      label: "environment-data:commands:thread:start-turn",
      execute: (
        input: StartThreadTurnInput & {
          readonly attachmentAuthority?: AttachmentAdmissionAuthority;
        },
      ) => {
        const { attachmentAuthority, ...command } = input;
        return attachmentAuthority === undefined
          ? startThreadTurn(input)
          : admitStagedThreadTurn(command, attachmentAuthority);
      },
      scheduler,
      concurrency,
    }),
    steerTurn: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:steer-turn",
      execute: (input: SteerThreadTurnInput) => steerThreadTurn(input),
      scheduler,
      concurrency,
    }),
    promoteTurn: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:promote-turn",
      execute: (input: PromoteThreadTurnInput) => promoteThreadTurn(input),
      scheduler,
      concurrency,
    }),
    interruptTurn: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:interrupt-turn",
      execute: (input: InterruptThreadTurnInput) => interruptThreadTurn(input),
      scheduler,
      concurrency,
    }),
    resolveDelivery: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:resolve-delivery",
      execute: (input: ResolveTurnDeliveryInput) => resolveTurnDelivery(input),
      scheduler,
      concurrency,
    }),
    respondToApproval: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:respond-to-approval",
      execute: (input: RespondToThreadApprovalInput) => respondToThreadApproval(input),
      scheduler,
      concurrency,
    }),
    respondToUserInput: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:respond-to-user-input",
      execute: (input: RespondToThreadUserInputInput) => respondToThreadUserInput(input),
      scheduler,
      concurrency,
    }),
    revertCheckpoint: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:revert-checkpoint",
      execute: (input: RevertThreadCheckpointInput) => revertThreadCheckpoint(input),
      scheduler,
      concurrency,
    }),
    stopSession: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:thread:stop-session",
      execute: (input: StopThreadSessionInput) => stopThreadSession(input),
      scheduler,
      concurrency,
    }),
  };
}
