import type { StagedUploadChatAttachment } from "@coddy/contracts";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { EnvironmentRpcUnavailableError } from "../rpc/client.ts";
import * as Effect from "effect/Effect";
import * as Cause from "effect/Cause";
import * as Fiber from "effect/Fiber";
import * as Option from "effect/Option";
import { isSessionTransportLoss } from "../connection/nextSession.ts";
import * as Exit from "effect/Exit";

import {
  cancelUploadBestEffort,
  createUploadPort,
  stageUpload,
  type UploadFailure,
  type UploadPort,
  type UploadProgress,
  type UploadSession,
  UploadClientError,
} from "./uploadStager.ts";

export interface AttachmentSource {
  readonly type: "image" | "file";
  readonly id: string;
  readonly name: string;
  readonly mimeType: string;
  readonly sizeBytes: number;
  readonly file: Blob;
}
export interface AttachmentUploadProgress extends UploadProgress {
  readonly fileName: string;
}
export interface AttachmentStagingInput {
  readonly attachments: ReadonlyArray<AttachmentSource>;
  readonly reusable?: ReadonlyArray<StagedUploadChatAttachment>;
  readonly onProgress?: (progress: AttachmentUploadProgress) => void;
}
export type AttachmentStagingResult =
  | { readonly _tag: "inline" }
  | {
      readonly _tag: "staged";
      readonly attachments: ReadonlyArray<StagedUploadChatAttachment>;
    };

export function encodedAttachmentCharacters(source: AttachmentSource): number {
  const mimeType = source.file.type || "application/octet-stream";
  return `data:${mimeType};base64,`.length + 4 * Math.ceil(source.file.size / 3);
}

export function shouldStageAttachments(
  capable: boolean,
  sources: ReadonlyArray<AttachmentSource>,
): boolean {
  return (
    capable &&
    sources.reduce((total, source) => total + encodedAttachmentCharacters(source), 0) > 256 * 1024
  );
}

/** One sequential, bounded sweep; the operation owner interrupts and joins this lifetime. */
export const keepStagedAttachmentsAliveWithPort = Effect.fn("Attachments.keepAliveWithPort")(
  function* (
    attachments: () => ReadonlyArray<Pick<StagedUploadChatAttachment, "uploadId">>,
    port: Pick<UploadPort, "initial">,
  ) {
    while (true) {
      yield* Effect.sleep("1 minute");
      yield* Effect.gen(function* () {
        const current = yield* port.initial;
        if (!current.capable) return;
        const ids = attachments().map((attachment) => attachment.uploadId);
        for (const uploadId of ids) yield* current.get({ uploadId }).pipe(Effect.ignoreCause);
      }).pipe(Effect.timeoutOption(5000), Effect.ignoreCause);
    }
  },
);

/** Keeps completed IDs alive through queued or outstanding admission, without owning retries. */
export const keepStagedAttachmentsAlive = Effect.fn("Attachments.keepAlive")(function* (
  attachments: ReadonlyArray<Pick<StagedUploadChatAttachment, "uploadId">>,
) {
  const registry = yield* EnvironmentRegistry;
  const known = yield* EnvironmentSupervisor;
  const initial = registry
    .run(known.target.environmentId, createUploadPort("attachmentStaging"))
    .pipe(
      Effect.mapError(
        () =>
          new EnvironmentRpcUnavailableError({
            environmentId: known.target.environmentId,
            message: `${known.target.label} is disconnected`,
          }),
      ),
      Effect.flatMap((port) => port.initial),
      Effect.provideService(EnvironmentRegistry, registry),
    );
  return yield* keepStagedAttachmentsAliveWithPort(() => attachments, { initial });
});

export const stageAttachmentsWithPort = Effect.fn("Attachments.stageWithPort")(function* (
  input: AttachmentStagingInput,
  port: UploadPort,
) {
  const initial = yield* port.initial;
  if (!shouldStageAttachments(initial.capable, input.attachments))
    return { _tag: "inline" } as const;
  const staged: StagedUploadChatAttachment[] = [...(input.reusable ?? [])];
  if (
    input.reusable &&
    (staged.length !== input.attachments.length ||
      staged.some((attachment, index) => {
        const source = input.attachments[index]!;
        return (
          attachment.id !== source.id ||
          attachment.type !== source.type ||
          attachment.name !== source.name ||
          attachment.mimeType !== source.mimeType ||
          attachment.sizeBytes !== source.file.size
        );
      }))
  )
    return yield* new UploadClientError({
      fileName: input.attachments[0]?.name ?? "attachment",
      reason: "protocol",
      message: "the files changed; attach them again",
    });
  const budgets = input.attachments.map(() => ({ remaining: 1 }));
  const totalBytes = input.attachments.reduce((sum, source) => sum + source.file.size, 0);
  let keeper: Fiber.Fiber<void, never> | null = null;
  let carryingSession = initial;
  const report = (fileName: string, sentBytes: number, phase: UploadProgress["phase"]) =>
    input.onProgress?.({ fileName, sentBytes, totalBytes, phase });
  const acknowledgedTotal = () => staged.reduce((sum, attachment) => sum + attachment.sizeBytes, 0);
  const uploadFile = Effect.fn("Attachments.uploadFile")(function* (index: number) {
    const attachment = input.attachments[index]!;
    const otherBytes = staged.reduce(
      (sum, item, itemIndex) => sum + (itemIndex === index ? 0 : item.sizeBytes),
      0,
    );
    report(attachment.name, otherBytes, "uploading");
    const upload = yield* stageUpload(
      {
        file: attachment.file,
        fileName: attachment.name,
        target: {
          _tag: "chat-attachment",
          type: attachment.type,
          name: attachment.name,
          mimeType: attachment.mimeType,
        },
        restartBudget: budgets[index]!,
        onProgress: (progress) =>
          report(attachment.name, otherBytes + progress.sentBytes, progress.phase),
      },
      port,
    );
    staged[index] = {
      type: attachment.type,
      id: attachment.id,
      name: attachment.name,
      mimeType: attachment.mimeType,
      sizeBytes: upload.sizeBytes,
      uploadId: upload.uploadId,
    };
  });
  const keepCompletedAlive = keepStagedAttachmentsAliveWithPort(() => staged, port);
  const validate = Effect.fn("Attachments.validateCompleted")(function* (index: number) {
    const attachment = staged[index]!;
    const fileName = input.attachments[index]!.name;
    const current = yield* Effect.exit(port.initial);
    if (Exit.isSuccess(current)) carryingSession = current.value;
    else {
      if (!isSessionTransportLoss(current.cause)) return yield* Effect.failCause(current.cause);
      report(fileName, acknowledgedTotal(), "reconnecting");
      carryingSession = yield* port.next(carryingSession);
    }
    const read = (session: UploadSession) =>
      session.capable
        ? session.get({ uploadId: attachment.uploadId }).pipe(Effect.timeoutOption(5000))
        : Effect.fail(
            new UploadClientError({
              fileName,
              reason: "unavailable",
              message: "this server cannot stage attachments; update it and try again",
            }),
          );
    let status = yield* Effect.exit(read(carryingSession));
    if (Exit.isFailure(status) && isSessionTransportLoss(status.cause)) {
      report(fileName, acknowledgedTotal(), "reconnecting");
      carryingSession = yield* port.next(carryingSession);
      status = yield* Effect.exit(read(carryingSession));
    }
    if (Exit.isFailure(status)) {
      const error = Cause.findErrorOption(status.cause);
      if (
        Option.isSome(error) &&
        error.value._tag === "UploadError" &&
        error.value.reason === "not_found" &&
        budgets[index]!.remaining > 0
      ) {
        budgets[index]!.remaining--;
        yield* uploadFile(index);
        return true;
      }
      return yield* Effect.failCause(status.cause);
    }
    if (Option.isNone(status.value))
      return yield* new UploadClientError({
        fileName,
        reason: "resume",
        message: "the upload could not be verified; try sending again",
      });
    const snapshot = status.value.value;
    if (
      snapshot.uploadId !== attachment.uploadId ||
      !snapshot.complete ||
      snapshot.sizeBytes !== attachment.sizeBytes ||
      snapshot.receivedBytes !== attachment.sizeBytes
    )
      return yield* new UploadClientError({
        fileName,
        reason: "protocol",
        message: "the server returned an incomplete upload; try sending again",
      });
    report(fileName, acknowledgedTotal(), "uploading");
    return false;
  });
  const run = Effect.gen(function* () {
    keeper = yield* keepCompletedAlive.pipe(Effect.forkChild);
    if (!input.reusable)
      for (let index = 0; index < input.attachments.length; index++) yield* uploadFile(index);
    // A restart during restaging may invalidate a previously verified file. Every restart
    // consumes that file's shared budget, so repeating validation is bounded by file count.
    let index = 0;
    while (index < staged.length) index = (yield* validate(index)) ? 0 : index + 1;
    return { _tag: "staged", attachments: staged } as const;
  });
  return yield* run.pipe(
    Effect.onExit(
      Effect.fn("Attachments.cleanupTurn")(function* (exit) {
        if (keeper) yield* Fiber.interrupt(keeper);
        if (Exit.isSuccess(exit)) return;
        const current = yield* Effect.exit(port.initial);
        if (Exit.isFailure(current)) return;
        yield* Effect.forEach(
          staged,
          (attachment) => cancelUploadBestEffort(current.value, attachment.uploadId),
          { concurrency: 8, discard: true },
        );
      }),
    ),
  );
});

export const stageAttachments = Effect.fn("Attachments.stage")(function* (
  input: AttachmentStagingInput,
) {
  if (input.attachments.length === 0) return { _tag: "inline" } as const;
  const port = yield* createUploadPort("attachmentStaging");
  return yield* stageAttachmentsWithPort(input, port);
});

/** An abandoned send releases its completed stages; failed turn admission may retain them for retry. */
export const releaseStagedAttachments = Effect.fn("Attachments.release")(
  function* (attachments: ReadonlyArray<Pick<StagedUploadChatAttachment, "uploadId">>) {
    if (attachments.length === 0) return;
    const port = yield* createUploadPort("attachmentStaging");
    const current = yield* port.initial;
    for (const attachment of attachments)
      yield* cancelUploadBestEffort(current, attachment.uploadId);
  },
  (effect) => Effect.ignoreCause(effect),
);

export type { UploadFailure };
