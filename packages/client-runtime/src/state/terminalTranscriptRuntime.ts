import type {
  TerminalAttachStreamEvent,
  TerminalConsoleTheme,
  TerminalSessionSnapshot,
  TerminalSize,
} from "@coddy/contracts";

import {
  createTerminalTranscript,
  DEFAULT_MAX_TERMINAL_BUFFER_BYTES,
} from "./terminalTranscript.ts";

export type TerminalRenderSignal =
  | {
      readonly type: "reset";
      readonly snapshot: string;
      readonly size?: TerminalSize;
      readonly oscColorResponderActive?: boolean;
      readonly firstAttachmentGrant?: boolean;
    }
  | { readonly type: "resized"; readonly size: TerminalSize }
  | { readonly type: "delta"; readonly data: string };

export interface TerminalMetadataSnapshot {
  readonly status: TerminalSessionSnapshot["status"] | "closed";
  readonly error: string | null;
  readonly consoleTheme: TerminalConsoleTheme | null;
  /** Bumps whenever a server snapshot replaces the transcript. */
  readonly generation: number;
  /** Bumps for lifecycle metadata changes, never for output, activity, or resizing. */
  readonly revision: number;
}

export const EMPTY_TERMINAL_METADATA_SNAPSHOT = Object.freeze<TerminalMetadataSnapshot>({
  status: "closed",
  error: null,
  consoleTheme: null,
  generation: 0,
  revision: 0,
});

export interface TerminalTranscriptRuntime {
  ingest(event: TerminalAttachStreamEvent): void;
  attachRenderer(
    sink: (signal: TerminalRenderSignal) => void,
    sizeClaim?: string,
  ): { detach(): void };
  snapshot(): string;
  metadata(): TerminalMetadataSnapshot;
  subscribeMetadata(listener: (metadata: TerminalMetadataSnapshot) => void): () => void;
}

export function createTerminalTranscriptRuntime(
  options: { readonly maxBufferBytes?: number } = {},
): TerminalTranscriptRuntime {
  const transcript = createTerminalTranscript(
    options.maxBufferBytes ?? DEFAULT_MAX_TERMINAL_BUFFER_BYTES,
  );
  const rendererClaims = new Map<(signal: TerminalRenderSignal) => void, string | undefined>();
  const metadataListeners = new Set<(metadata: TerminalMetadataSnapshot) => void>();
  const pendingEvents: Array<TerminalAttachStreamEvent> = [];
  let pendingHead = 0;
  let processingEvents = false;
  let metadata = EMPTY_TERMINAL_METADATA_SNAPSHOT;
  let size: TerminalSize | undefined;
  let oscColorResponderActive = false;
  let firstAttachmentGrantClaim: string | null = null;
  let renderers: ReadonlyArray<
    readonly [(signal: TerminalRenderSignal) => void, string | undefined]
  > = [];

  const fanRender = (signal: TerminalRenderSignal | ((claim?: string) => TerminalRenderSignal)) => {
    const currentRenderers = renderers;
    for (const [sink, claim] of currentRenderers) {
      try {
        sink(typeof signal === "function" ? signal(claim) : signal);
      } catch {
        // Renderer failures are isolated from transcript ingestion and other renderers.
      }
    }
  };

  const notifyMetadata = () => {
    for (const listener of Array.from(metadataListeners)) {
      try {
        listener(metadata);
      } catch {
        // A faulty observer must not prevent other observers from receiving lifecycle state.
      }
    }
  };

  const updateMetadata = (
    next: Pick<TerminalMetadataSnapshot, "status" | "error"> & {
      readonly consoleTheme?: TerminalConsoleTheme | null;
    },
    newGeneration: boolean,
  ) => {
    metadata = Object.freeze({
      status: next.status,
      error: next.error,
      consoleTheme: next.consoleTheme === undefined ? metadata.consoleTheme : next.consoleTheme,
      generation: metadata.generation + (newGeneration ? 1 : 0),
      revision: metadata.revision + 1,
    });
  };

  const createResetSignalAndConsumeGrant = (
    snapshot: string,
    rendererClaim?: string,
  ): TerminalRenderSignal => {
    const firstAttachmentGrant =
      firstAttachmentGrantClaim !== null && firstAttachmentGrantClaim === rendererClaim;
    // A cached transcript can hydrate many mounts. The first-attachment grant
    // is consumed only by its owner, once, rather than replayed with the cache.
    if (firstAttachmentGrant) firstAttachmentGrantClaim = null;
    return {
      type: "reset",
      snapshot,
      ...(size ? { size } : {}),
      ...(oscColorResponderActive ? { oscColorResponderActive: true } : {}),
      ...(firstAttachmentGrant ? { firstAttachmentGrant: true } : {}),
    };
  };

  /**
   * Replaces the transcript and state with `snapshot`'s. A resync (the server's catch-up
   * after its stream fell behind) is the same attachment and process, so it keeps the
   * generation, and with it any input still waiting to be written.
   */
  const resetTranscript = (snapshot: TerminalSessionSnapshot, newGeneration = true) => {
    transcript.clear();
    transcript.append(snapshot.history);
    size = snapshot.size;
    oscColorResponderActive = snapshot.oscColorResponderActive ?? false;
    // A resync is the same attachment: a startup grant not used yet stays its owner's.
    if (newGeneration) {
      firstAttachmentGrantClaim = snapshot.firstAttachmentGrant ? (size?.sizeClaim ?? null) : null;
    }
    updateMetadata(
      { status: snapshot.status, error: null, consoleTheme: snapshot.consoleTheme ?? null },
      newGeneration,
    );
    const history = transcript.snapshot();
    fanRender((claim) => createResetSignalAndConsumeGrant(history, claim));
    notifyMetadata();
  };

  const processEvent = (event: TerminalAttachStreamEvent) => {
    switch (event.type) {
      case "snapshot":
        resetTranscript(event.snapshot, event.resync !== true);
        return;
      case "restarted":
        resetTranscript(event.snapshot);
        return;
      case "output":
        transcript.append(event.data);
        fanRender({ type: "delta", data: event.data });
        return;
      case "resized":
        size = event.size;
        if (size.sizeClaim !== firstAttachmentGrantClaim) firstAttachmentGrantClaim = null;
        fanRender({ type: "resized", size });
        return;
      case "cleared":
        transcript.clear();
        firstAttachmentGrantClaim = null;
        updateMetadata({ status: metadata.status, error: null }, false);
        fanRender((claim) => createResetSignalAndConsumeGrant("", claim));
        notifyMetadata();
        return;
      case "exited":
        updateMetadata({ status: "exited", error: null }, false);
        notifyMetadata();
        return;
      case "closed":
        updateMetadata({ status: "closed", error: null }, false);
        notifyMetadata();
        return;
      case "error":
        updateMetadata({ status: "error", error: event.message }, false);
        notifyMetadata();
        return;
      case "activity":
        return;
    }
  };

  return {
    ingest(event) {
      pendingEvents.push(event);
      if (processingEvents) return;

      processingEvents = true;
      try {
        while (pendingHead < pendingEvents.length) {
          processEvent(pendingEvents[pendingHead]!);
          pendingHead += 1;
        }
      } finally {
        pendingEvents.length = 0;
        pendingHead = 0;
        processingEvents = false;
      }
    },
    attachRenderer(sink, rendererClaim) {
      let attached = true;
      rendererClaims.set(sink, rendererClaim);
      renderers = Array.from(rendererClaims);
      try {
        sink(createResetSignalAndConsumeGrant(transcript.snapshot(), rendererClaim));
      } catch {
        // Initial hydration obeys the same observer isolation as live fan-out.
      }
      return {
        detach() {
          if (!attached) return;
          attached = false;
          rendererClaims.delete(sink);
          renderers = Array.from(rendererClaims);
        },
      };
    },
    snapshot() {
      return transcript.snapshot();
    },
    metadata() {
      return metadata;
    },
    subscribeMetadata(listener) {
      let subscribed = true;
      metadataListeners.add(listener);
      return () => {
        if (!subscribed) return;
        subscribed = false;
        metadataListeners.delete(listener);
      };
    },
  };
}
