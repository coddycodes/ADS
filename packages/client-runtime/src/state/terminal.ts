import {
  type EnvironmentId,
  type TerminalImagePasteMimeType,
  type TerminalMetadataStreamEvent,
  type TerminalSummary,
  type TerminalSessionSnapshot,
  WS_METHODS,
} from "@bibcode/contracts";
import * as Effect from "effect/Effect";
import * as Stream from "effect/Stream";
import { Atom } from "effect/reactivity";

import {
  createAtomCommandScheduler,
  createEnvironmentCommand,
  createEnvironmentRpcCommand,
  createEnvironmentRpcSubscriptionAtomFamily,
  createEnvironmentSubscriptionAtomFamily,
  environmentRpcKey,
  followStreamInEnvironment,
  parseEnvironmentRpcKey,
  runInEnvironment,
} from "./runtime.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";
import {
  currentSession,
  request,
  requestInSession,
  EnvironmentRpcUnavailableError,
  subscribe,
  type EnvironmentRpcInput,
} from "../rpc/client.ts";
import {
  acquireTerminalMetadataStream,
  createTerminalTranscriptRuntimeRegistry,
  terminalTranscriptRuntimeKey,
} from "./terminalAttachAdapter.ts";
import { applyTerminalMetadataStreamEvent } from "./terminalSession.ts";
import {
  EMPTY_TERMINAL_METADATA_SNAPSHOT,
  type TerminalMetadataSnapshot,
  type TerminalTranscriptRuntime,
} from "./terminalTranscriptRuntime.ts";

import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { releaseStagedAttachments } from "../operations/attachmentStaging.ts";
import { createUploadPort, stageUpload } from "../operations/uploadStager.ts";
import {
  createTerminalInputBindingRegistry,
  type TerminalInputTarget,
} from "./orderedTerminalInput.ts";

export interface TerminalAttachSnapshot {
  readonly metadata: TerminalMetadataSnapshot;
  readonly transcriptRuntime: TerminalTranscriptRuntime | null;
}

export const EMPTY_TERMINAL_ATTACH_SNAPSHOT = Object.freeze<TerminalAttachSnapshot>({
  metadata: EMPTY_TERMINAL_METADATA_SNAPSHOT,
  transcriptRuntime: null,
});

export function accumulateTerminalMetadataEvents<E, R>(
  events: Stream.Stream<TerminalMetadataStreamEvent, E, R>,
) {
  return events.pipe(
    Stream.scan((): ReadonlyArray<TerminalSummary> => [], applyTerminalMetadataStreamEvent),
    Stream.drop(1),
  );
}

export interface TerminalImagePasteInput {
  readonly file: Blob;
  readonly name: string;
  readonly mimeType: TerminalImagePasteMimeType;
}

/** Uploads a pasted image and returns the server path of its copy for the terminal program. */
export const stageTerminalImagePaste = Effect.fn("terminal.stageImagePaste")(function* (
  input: TerminalImagePasteInput,
) {
  const environmentId = (yield* EnvironmentSupervisor).target.environmentId;
  const port = yield* createUploadPort("attachmentStaging");
  const staged = yield* stageUpload(
    {
      target: {
        _tag: "chat-attachment",
        type: "image",
        name: input.name,
        mimeType: input.mimeType,
      },
      file: input.file,
      fileName: input.name,
    },
    port,
  );
  // The upload may have resumed on a replacement connection, so the commit (and
  // the release of a stage it did not consume) resolve the current one.
  const { path } = yield* runInEnvironment(
    environmentId,
    request(WS_METHODS.terminalStageImagePaste, {
      uploadId: staged.uploadId,
      name: input.name,
      mimeType: input.mimeType,
      sizeBytes: staged.sizeBytes,
    }),
  ).pipe(
    Effect.onError(() =>
      runInEnvironment(environmentId, releaseStagedAttachments([staged])).pipe(Effect.ignore),
    ),
  );
  return path;
});

export function createTerminalEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
) {
  const inputBindings = createTerminalInputBindingRegistry();
  const prepareConfirmedInput = Effect.fn("terminal.prepareConfirmedInput")(function* (
    session: RpcSession,
    environmentId: string,
    snapshot: TerminalSessionSnapshot,
  ) {
    if ((yield* currentSession()) !== session) {
      return yield* new EnvironmentRpcUnavailableError({
        environmentId,
        message: "Terminal connection changed during the operation. Reattach before typing again.",
      });
    }
    yield* inputBindings.prepare(
      session,
      environmentId,
      { threadId: snapshot.threadId, terminalId: snapshot.terminalId },
      yield* Effect.context(),
    );
    return snapshot;
  });
  const lifecycleScheduler = createAtomCommandScheduler();
  const resizeScheduler = createAtomCommandScheduler();
  const transcriptRuntimes = createTerminalTranscriptRuntimeRegistry();
  const terminalThreadKey = ({
    environmentId,
    input,
  }: {
    readonly environmentId: string;
    readonly input: { readonly threadId: string; readonly terminalId?: string | undefined };
  }) => JSON.stringify([environmentId, input.threadId]);
  const terminalSessionKey = ({
    environmentId,
    input,
  }: {
    readonly environmentId: string;
    readonly input: { readonly threadId: string; readonly terminalId?: string | undefined };
  }) => JSON.stringify([environmentId, input.threadId, input.terminalId ?? null]);
  const lifecycleConcurrency = { mode: "serial" as const, key: terminalThreadKey };
  const attachSnapshots = Atom.family((key: string) =>
    Atom.make<TerminalAttachSnapshot>(EMPTY_TERMINAL_ATTACH_SNAPSHOT).pipe(
      Atom.withLabel(`environment-data:terminal:attach-snapshot:${key}`),
    ),
  );
  const attachSnapshot = (target: {
    readonly environmentId: EnvironmentId;
    readonly input: EnvironmentRpcInput<typeof WS_METHODS.terminalAttach>;
  }) => attachSnapshots(environmentRpcKey(target));
  const attachProducer = (() => {
    const family = Atom.family((key: string) => {
      const target =
        parseEnvironmentRpcKey<EnvironmentRpcInput<typeof WS_METHODS.terminalAttach>>(key);
      const runtimeKey = terminalTranscriptRuntimeKey(target.environmentId, target.input);
      return runtime
        .atom((get) => {
          const atomRegistry = get.registry;
          const snapshotAtom = attachSnapshots(key);
          return acquireTerminalMetadataStream(
            followStreamInEnvironment(
              target.environmentId,
              subscribe(WS_METHODS.terminalAttach, target.input),
            ),
            transcriptRuntimes,
            runtimeKey,
          ).pipe(
            Stream.tap((metadata) =>
              Effect.sync(() => {
                const transcriptRuntime = transcriptRuntimes.get(runtimeKey);
                if (transcriptRuntime !== undefined) {
                  atomRegistry.set(snapshotAtom, { metadata, transcriptRuntime });
                }
              }),
            ),
            Stream.ensuring(
              Effect.sync(() => {
                atomRegistry.set(snapshotAtom, EMPTY_TERMINAL_ATTACH_SNAPSHOT);
              }),
            ),
          );
        })
        .pipe(Atom.setIdleTTL(0), Atom.withLabel(`environment-data:terminal:attach:${key}`));
    });
    return (target: {
      readonly environmentId: EnvironmentId;
      readonly input: EnvironmentRpcInput<typeof WS_METHODS.terminalAttach>;
    }) => family(environmentRpcKey(target));
  })();
  return {
    attachProducer,
    attachSnapshot,
    transcriptRuntimes,
    events: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:terminal:events",
      tag: WS_METHODS.subscribeTerminalEvents,
    }),
    /**
     * Raw metadata stream for consumers that must tell explicit removals from
     * snapshots. Each value is a whole delivered batch: an atom keeps only the
     * last element of a chunk, which would drop removals.
     */
    metadataEvents: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:terminal:metadata-events",
      tag: WS_METHODS.subscribeTerminalMetadata,
      transform: Stream.chunks,
    }),
    metadata: createEnvironmentSubscriptionAtomFamily(runtime, {
      label: "environment-data:terminal:metadata",
      subscribe: (_input: null) =>
        accumulateTerminalMetadataEvents(subscribe(WS_METHODS.subscribeTerminalMetadata, {})),
    }),
    open: createEnvironmentCommand(runtime, {
      label: "environment-data:terminal:open",
      execute: Effect.fn("terminal.open")(function* (
        input: EnvironmentRpcInput<typeof WS_METHODS.terminalOpen>,
      ) {
        const supervisor = yield* EnvironmentSupervisor;
        const session = yield* currentSession();
        const snapshot = yield* requestInSession(
          session,
          supervisor.target.environmentId,
          WS_METHODS.terminalOpen,
          input,
        );
        return yield* prepareConfirmedInput(session, supervisor.target.environmentId, snapshot);
      }),
      scheduler: lifecycleScheduler,
      concurrency: lifecycleConcurrency,
    }),
    prepareInput: createEnvironmentCommand(runtime, {
      label: "environment-data:terminal:prepare-input",
      execute: Effect.fn("terminal.prepareInput")(function* (input: TerminalInputTarget) {
        const supervisor = yield* EnvironmentSupervisor;
        const session = yield* currentSession();
        return yield* inputBindings.prepare(
          session,
          supervisor.target.environmentId,
          input,
          yield* Effect.context(),
        );
      }),
    }),
    resetInput: createEnvironmentCommand(runtime, {
      label: "environment-data:terminal:reset-input",
      execute: Effect.fn("terminal.resetInput")(function* (input: TerminalInputTarget) {
        const supervisor = yield* EnvironmentSupervisor;
        inputBindings.reset(supervisor.target.environmentId, input);
      }),
    }),
    write: createEnvironmentCommand(runtime, {
      label: "environment-data:terminal:write",
      execute: Effect.fn("terminal.write")(function* (
        input: EnvironmentRpcInput<typeof WS_METHODS.terminalWrite>,
      ) {
        const supervisor = yield* EnvironmentSupervisor;
        const session = yield* currentSession();
        return yield* inputBindings.write(
          session,
          supervisor.target.environmentId,
          input,
          input.data,
          yield* Effect.context(),
        );
      }),
    }),
    resize: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:terminal:resize",
      tag: WS_METHODS.terminalResize,
      scheduler: resizeScheduler,
      concurrency: { mode: "latest", key: terminalSessionKey },
    }),
    clear: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:terminal:clear",
      tag: WS_METHODS.terminalClear,
      scheduler: lifecycleScheduler,
      concurrency: lifecycleConcurrency,
    }),
    restart: createEnvironmentCommand(runtime, {
      label: "environment-data:terminal:restart",
      execute: Effect.fn("terminal.restart")(function* (
        input: EnvironmentRpcInput<typeof WS_METHODS.terminalRestart>,
      ) {
        const supervisor = yield* EnvironmentSupervisor;
        const session = yield* currentSession();
        inputBindings.reset(supervisor.target.environmentId, input);
        const snapshot = yield* requestInSession(
          session,
          supervisor.target.environmentId,
          WS_METHODS.terminalRestart,
          input,
        );
        return yield* prepareConfirmedInput(session, supervisor.target.environmentId, snapshot);
      }),
      scheduler: lifecycleScheduler,
      concurrency: lifecycleConcurrency,
    }),
    stageImagePaste: createEnvironmentCommand<
      EnvironmentRegistry | R,
      E,
      TerminalImagePasteInput,
      Effect.Success<ReturnType<typeof stageTerminalImagePaste>>,
      Effect.Error<ReturnType<typeof stageTerminalImagePaste>>
    >(runtime, {
      label: "environment-data:terminal:stage-image-paste",
      execute: stageTerminalImagePaste,
    }),
    close: createEnvironmentCommand(runtime, {
      label: "environment-data:terminal:close",
      execute: Effect.fn("terminal.close")(function* (
        input: EnvironmentRpcInput<typeof WS_METHODS.terminalClose>,
      ) {
        const supervisor = yield* EnvironmentSupervisor;
        inputBindings.reset(supervisor.target.environmentId, input);
        return yield* request(WS_METHODS.terminalClose, input);
      }),
      scheduler: lifecycleScheduler,
      concurrency: lifecycleConcurrency,
    }),
  };
}

export * from "./terminalSession.ts";
export * from "./terminalInput.ts";
export * from "./terminalAttachAdapter.ts";
export * from "./terminalTranscriptRuntime.ts";
export { createTerminalTranscript, type TerminalTranscript } from "./terminalTranscript.ts";
