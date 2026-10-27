import { EnvironmentId, WS_METHODS } from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { vi } from "vite-plus/test";

import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { AVAILABLE_CONNECTION_STATE, type SupervisorConnectionState } from "../connection/model.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import { createUploadPort } from "./uploadStager.ts";

function session(capable: boolean) {
  const begin = vi.fn(() => Effect.succeed({ uploadId: "stage", exists: false }));
  const rpc: RpcSession = {
    client: {
      [WS_METHODS.uploadsBegin]: begin,
      [WS_METHODS.uploadsAppend]: () => Effect.succeed({ receivedBytes: 3 }),
      [WS_METHODS.uploadsGet]: () =>
        Effect.succeed({ uploadId: "stage", receivedBytes: 3, sizeBytes: 3, complete: true }),
      [WS_METHODS.uploadsCancel]: () => Effect.succeed({}),
    } as unknown as WsRpcProtocolClient,
    initialConfig: Effect.succeed({
      environment: { capabilities: { attachmentStaging: capable } },
    } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
  return { rpc, begin };
}

describe("upload session port", () => {
  it.effect(
    "takes capability from the carrying session and keeps every request on its exact session",
    () =>
      Effect.gen(function* () {
        const first = session(true);
        const replacement = session(false);
        const target = { environmentId: EnvironmentId.make("host"), label: "Build server" };
        const sessions = yield* SubscriptionRef.make(Option.some(first.rpc));
        const states = yield* SubscriptionRef.make<SupervisorConnectionState>({
          ...AVAILABLE_CONNECTION_STATE,
          desired: true,
          phase: "connected",
        });
        const supervisor = EnvironmentSupervisor.of({
          target,
          session: sessions,
          state: states,
          prepared: { descriptor: { capabilities: { attachmentStaging: true } } },
        } as never);
        const entries = yield* SubscriptionRef.make(new Map([[target.environmentId, { target }]]));
        const registry = EnvironmentRegistry.of({
          entries,
          followStream: (
            _id: string,
            stream: Stream.Stream<unknown, unknown, EnvironmentSupervisor>,
          ) => Stream.provideService(stream, EnvironmentSupervisor, supervisor),
        } as never);
        const port = yield* createUploadPort("attachmentStaging").pipe(
          Effect.provideService(EnvironmentRegistry, registry),
          Effect.provideService(EnvironmentSupervisor, supervisor),
        );
        const carrying = yield* port.initial;
        expect(carrying.capable).toBe(true);
        yield* SubscriptionRef.set(sessions, Option.some(replacement.rpc));
        const next = yield* port.next(carrying);
        expect(next.identity).toBe(replacement.rpc);
        expect(next.capable).toBe(false);
        yield* carrying.begin({
          target: { _tag: "chat-attachment", type: "file", name: "a.txt", mimeType: "text/plain" },
          sizeBytes: 3,
        });
        expect(first.begin).toHaveBeenCalledOnce();
        expect(replacement.begin).not.toHaveBeenCalled();
        yield* SubscriptionRef.set(states, { ...AVAILABLE_CONNECTION_STATE, desired: false });
        yield* SubscriptionRef.set(sessions, Option.none());
        const stopped = yield* Effect.exit(port.next(next));
        expect(Exit.isFailure(stopped)).toBe(true);
      }),
  );
});
