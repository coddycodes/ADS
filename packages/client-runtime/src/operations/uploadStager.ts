import {
  type UploadBeginInput,
  type UploadBeginResult,
  type UploadAppendInput,
  type UploadAppendResult,
  type UploadGetInput,
  type UploadGetResult,
  type UploadCancelInput,
  type UploadCancelResult,
  type UploadTarget,
  WS_METHODS,
} from "@coddy/contracts";
import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as Base64 from "effect/encoding/Base64";
import * as Exit from "effect/Exit";
import * as Fiber from "effect/Fiber";
import * as Schema from "effect/Schema";

import { isSessionTransportLoss, nextSession } from "../connection/nextSession.ts";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import {
  currentSession,
  requestInSession,
  EnvironmentRpcUnavailableError,
  type EnvironmentRpcFailure,
} from "../rpc/client.ts";
import type { RpcSession } from "../rpc/session.ts";

export interface UploadProgress {
  readonly sentBytes: number;
  readonly totalBytes: number;
  readonly phase: "uploading" | "reconnecting";
}

export class UploadClientError extends Schema.TaggedError<UploadClientError>()(
  "UploadClientError",
  {
    fileName: Schema.String,
    reason: Schema.Literals(["unavailable", "read", "resume", "protocol"]),
    message: Schema.String,
  },
) {}

export type UploadFailure =
  | EnvironmentRpcFailure<
      | typeof WS_METHODS.uploadsBegin
      | typeof WS_METHODS.uploadsAppend
      | typeof WS_METHODS.uploadsGet
      | typeof WS_METHODS.uploadsCancel
    >
  | UploadClientError
  | EnvironmentRpcUnavailableError;

export interface UploadSession {
  readonly identity: object;
  readonly capable: boolean;
  readonly begin: (input: UploadBeginInput) => Effect.Effect<UploadBeginResult, UploadFailure>;
  readonly append: (input: UploadAppendInput) => Effect.Effect<UploadAppendResult, UploadFailure>;
  readonly get: (input: UploadGetInput) => Effect.Effect<UploadGetResult, UploadFailure>;
  readonly cancel: (input: UploadCancelInput) => Effect.Effect<UploadCancelResult, UploadFailure>;
}
export interface UploadPort {
  readonly initial: Effect.Effect<UploadSession, UploadFailure>;
  readonly next: (lost: UploadSession) => Effect.Effect<UploadSession, UploadFailure>;
}
export interface StageUploadInput {
  readonly target: UploadTarget;
  readonly file: Blob;
  readonly fileName: string;
  readonly onProgress?: (progress: UploadProgress) => void;
  readonly beforeBytes?: (begun: UploadBeginResult) => Effect.Effect<void, UploadFailure>;
  readonly restartBudget?: { remaining: number };
}
export interface StagedUpload {
  readonly uploadId: string;
  readonly sizeBytes: number;
}
export type UploadCapability = "attachmentStaging";

/** Every request stays bound to the carrying session; the registry alone owns reconnects. */
export const createUploadPort = Effect.fn("Uploads.createPort")(function* (
  capability: UploadCapability,
) {
  const registry = yield* EnvironmentRegistry;
  let supervisor = yield* EnvironmentSupervisor;
  const bind = Effect.fn("Uploads.bindSession")(function* (session: RpcSession) {
    const config = yield* session.initialConfig.pipe(
      Effect.mapError(
        () =>
          new EnvironmentRpcUnavailableError({
            environmentId: supervisor.target.environmentId,
            message: `${supervisor.target.label} is disconnected`,
          }),
      ),
    );
    const environmentId = supervisor.target.environmentId;
    return {
      identity: session,
      capable: config.environment.capabilities[capability] === true,
      begin: (input) => requestInSession(session, environmentId, WS_METHODS.uploadsBegin, input),
      append: (input) => requestInSession(session, environmentId, WS_METHODS.uploadsAppend, input),
      get: (input) => requestInSession(session, environmentId, WS_METHODS.uploadsGet, input),
      cancel: (input) => requestInSession(session, environmentId, WS_METHODS.uploadsCancel, input),
    } satisfies UploadSession;
  });
  return {
    initial: Effect.suspend(() =>
      currentSession().pipe(
        Effect.provideService(EnvironmentSupervisor, supervisor),
        Effect.flatMap(bind),
      ),
    ),
    next: Effect.fn("Uploads.nextSession")(function* (lost: UploadSession) {
      const next = yield* nextSession(registry, supervisor, lost.identity as RpcSession);
      if (next._tag === "Stopped")
        return yield* new EnvironmentRpcUnavailableError({
          environmentId: supervisor.target.environmentId,
          message: `${next.label} is disconnected`,
        });
      supervisor = next.supervisor;
      return yield* bind(next.session);
    }),
  } satisfies UploadPort;
});

export function nextUploadChunkBytes(size: number, rttMs: number): number {
  const desired = (size * 1000) / Math.max(1, rttMs);
  return Math.floor(
    Math.max(16 * 1024, Math.min(1024 * 1024, size * 2, Math.max(size / 2, desired))),
  );
}

export const cancelUploadBestEffort = Effect.fn("Uploads.cancelBestEffort")(function* (
  session: UploadSession,
  uploadId: string,
) {
  if (!session.capable) return;
  yield* session.cancel({ uploadId }).pipe(Effect.timeoutOption(1000), Effect.ignoreCause);
});

type Hash = ReturnType<typeof sha256.create>;
interface PendingChunk {
  readonly end: number;
  readonly size: number;
  readonly fiber: Fiber.Fiber<
    { readonly receivedBytes: number; readonly rttMs: number },
    UploadFailure
  >;
}

/** Two bounded source slices and hasher checkpoints let acknowledgements pace the uplink. */
export const stageUpload = Effect.fn("Uploads.stage")(function* (
  input: StageUploadInput,
  port: UploadPort,
) {
  const totalBytes = input.file.size;
  const clientError = (reason: UploadClientError["reason"], message: string) =>
    new UploadClientError({
      fileName: input.fileName,
      reason,
      message,
    });
  let session = yield* port.initial;
  let uploadId: string | null = null;
  let sentBytes = 0;
  let offset = 0;
  let chunkBytes = 64 * 1024;
  let hash = sha256.create();
  const restartBudget = input.restartBudget ?? { remaining: 1 };
  let complete = false;
  let emptyChunkPending = totalBytes === 0;
  const checkpoints = new Map<number, Hash>([[0, hash.clone()]]);
  const pending: PendingChunk[] = [];
  const report = (phase: UploadProgress["phase"]) =>
    input.onProgress?.({ sentBytes, totalBytes, phase });
  const requireCapability = () =>
    session.capable
      ? Effect.void
      : Effect.fail(
          clientError(
            "unavailable",
            "this server cannot stage attachments; update it and try again",
          ),
        );
  const stopPending = Effect.fn("Uploads.stopPending")(function* () {
    yield* Fiber.interruptAll(pending.map((chunk) => chunk.fiber));
    pending.length = 0;
  });
  const begin = Effect.fn("Uploads.beginFile")(function* () {
    yield* requireCapability();
    const begun = yield* session.begin({ target: input.target, sizeBytes: totalBytes });
    uploadId = begun.uploadId;
    if (input.beforeBytes) yield* input.beforeBytes(begun);
    sentBytes = 0;
    offset = 0;
    emptyChunkPending = totalBytes === 0;
    hash = sha256.create();
    checkpoints.clear();
    checkpoints.set(0, hash.clone());
    report("uploading");
  });
  const resume = Effect.fn("Uploads.resume")(function* (cause: Cause.Cause<UploadFailure>) {
    yield* stopPending();
    if (!isSessionTransportLoss(cause)) return yield* Effect.failCause(cause);
    report("reconnecting");
    while (true) {
      session = yield* port.next(session);
      yield* requireCapability();
      const status = yield* Effect.exit(session.get({ uploadId: uploadId! }));
      if (Exit.isFailure(status)) {
        const error = Cause.findErrorOption(status.cause);
        if (
          error._tag === "Some" &&
          error.value._tag === "UploadError" &&
          error.value.reason === "not_found"
        ) {
          if (restartBudget.remaining === 0) return yield* Effect.failCause(status.cause);
          restartBudget.remaining--;
          yield* begin();
          return;
        }
        if (isSessionTransportLoss(status.cause)) continue;
        return yield* Effect.failCause(status.cause);
      }
      const { receivedBytes } = status.value;
      if (
        status.value.uploadId !== uploadId ||
        status.value.sizeBytes !== totalBytes ||
        receivedBytes > totalBytes ||
        receivedBytes < sentBytes
      ) {
        return yield* clientError(
          "protocol",
          "the server returned an invalid upload checkpoint; try sending again",
        );
      }
      const checkpoint = checkpoints.get(receivedBytes);
      if (!checkpoint)
        return yield* clientError(
          "resume",
          "the upload could not resume safely; try sending again",
        );
      if (status.value.complete && receivedBytes !== totalBytes)
        return yield* clientError(
          "protocol",
          "the server returned an incomplete upload; try sending again",
        );
      sentBytes = receivedBytes;
      offset = receivedBytes;
      hash = checkpoint.clone();
      checkpoints.clear();
      checkpoints.set(receivedBytes, hash.clone());
      complete = status.value.complete;
      report("uploading");
      return;
    }
  });
  const run = Effect.gen(function* () {
    const initialBegin = yield* Effect.exit(begin());
    if (Exit.isFailure(initialBegin)) {
      if (
        uploadId !== null ||
        !isSessionTransportLoss(initialBegin.cause) ||
        restartBudget.remaining === 0
      )
        return yield* Effect.failCause(initialBegin.cause);
      restartBudget.remaining--;
      report("reconnecting");
      session = yield* port.next(session);
      yield* begin();
    }
    while (!complete) {
      while (pending.length < 2 && (offset < totalBytes || emptyChunkPending)) {
        const start = offset;
        const end = Math.min(totalBytes, start + chunkBytes);
        const bytes = new Uint8Array(
          yield* Effect.tryPromise({
            try: () => input.file.slice(start, end).arrayBuffer(),
            catch: () => clientError("read", "the file could not be read; attach it again"),
          }),
        );
        hash.update(bytes);
        checkpoints.set(end, hash.clone());
        offset = end;
        const append = {
          uploadId: uploadId!,
          offset: start,
          data: Base64.encode(bytes),
          ...(end === totalBytes ? { sha256: bytesToHex(hash.clone().digest()) } : {}),
        };
        const carryingSession = session;
        const fiber = yield* Effect.gen(function* () {
          const started = yield* Clock.currentTimeMillis;
          const result = yield* carryingSession.append(append);
          return { ...result, rttMs: (yield* Clock.currentTimeMillis) - started };
        }).pipe(Effect.forkChild);
        pending.push({ end, size: bytes.length, fiber });
        emptyChunkPending = false;
      }
      const first = pending[0]!;
      const result = yield* Fiber.await(first.fiber);
      if (Exit.isFailure(result)) {
        yield* resume(result.cause);
        continue;
      }
      pending.shift();
      const acknowledged = result.value.receivedBytes;
      if (acknowledged < first.end || acknowledged > offset || !checkpoints.has(acknowledged)) {
        return yield* clientError(
          "protocol",
          "the server returned an invalid upload acknowledgement; try sending again",
        );
      }
      sentBytes = Math.max(sentBytes, acknowledged);
      chunkBytes = nextUploadChunkBytes(first.size || chunkBytes, result.value.rttMs);
      for (const boundary of checkpoints.keys())
        if (boundary < sentBytes) checkpoints.delete(boundary);
      report("uploading");
      if (sentBytes === totalBytes && pending.length === 0) complete = true;
    }
    return { uploadId: uploadId!, sizeBytes: totalBytes } satisfies StagedUpload;
  });
  return yield* run.pipe(
    Effect.onExit(
      Effect.fn("Uploads.cleanup")(function* (exit) {
        yield* stopPending();
        if (Exit.isFailure(exit) && uploadId !== null)
          yield* cancelUploadBestEffort(session, uploadId);
      }),
    ),
  );
});
