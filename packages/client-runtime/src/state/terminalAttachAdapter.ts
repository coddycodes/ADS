import * as Effect from "effect/Effect";
import * as Result from "effect/Result";
import * as Stream from "effect/Stream";

import type { TerminalAttachStreamEvent } from "@coddy/contracts";

import {
  createTerminalTranscriptRuntime,
  type TerminalMetadataSnapshot,
  type TerminalTranscriptRuntime,
} from "./terminalTranscriptRuntime.ts";

function eventChangesMetadata(event: TerminalAttachStreamEvent): boolean {
  switch (event.type) {
    case "output":
    case "activity":
    case "resized":
      return false;
    case "snapshot":
    case "restarted":
    case "cleared":
    case "exited":
    case "closed":
    case "error":
      return true;
  }
}

export function projectTerminalMetadataStream<E, R>(
  events: Stream.Stream<TerminalAttachStreamEvent, E, R>,
  runtime: TerminalTranscriptRuntime,
): Stream.Stream<TerminalMetadataSnapshot, E, R> {
  return events.pipe(
    Stream.filterMap((event) => {
      runtime.ingest(event);
      return eventChangesMetadata(event) ? Result.succeed(runtime.metadata()) : Result.failVoid;
    }),
  );
}

export interface TerminalTranscriptRuntimeRegistry {
  acquire(key: string): TerminalTranscriptRuntime;
  release(key: string): void;
  get(key: string): TerminalTranscriptRuntime | undefined;
  referenceCount(key: string): number;
  size(): number;
}

export function createTerminalTranscriptRuntimeRegistry(): TerminalTranscriptRuntimeRegistry {
  const entries = new Map<string, { runtime: TerminalTranscriptRuntime; references: number }>();

  return {
    acquire(key) {
      const existing = entries.get(key);
      if (existing !== undefined) {
        existing.references += 1;
        return existing.runtime;
      }

      const runtime = createTerminalTranscriptRuntime();
      entries.set(key, { runtime, references: 1 });
      return runtime;
    },
    release(key) {
      const existing = entries.get(key);
      if (existing === undefined) return;

      existing.references -= 1;
      if (existing.references === 0) {
        entries.delete(key);
      }
    },
    get(key) {
      return entries.get(key)?.runtime;
    },
    referenceCount(key) {
      return entries.get(key)?.references ?? 0;
    },
    size() {
      return entries.size;
    },
  };
}

export function acquireTerminalMetadataStream<E, R>(
  events: Stream.Stream<TerminalAttachStreamEvent, E, R>,
  registry: TerminalTranscriptRuntimeRegistry,
  key: string,
): Stream.Stream<TerminalMetadataSnapshot, E, R> {
  return Stream.unwrap(
    Effect.acquireRelease(
      Effect.sync(() => registry.acquire(key)),
      () => Effect.sync(() => registry.release(key)),
    ).pipe(Effect.map((runtime) => projectTerminalMetadataStream(events, runtime))),
  );
}

export function terminalTranscriptRuntimeKey(
  environmentId: string,
  input: { readonly threadId: string; readonly terminalId?: string | undefined },
): string {
  return JSON.stringify([environmentId, input.threadId, input.terminalId ?? null]);
}
