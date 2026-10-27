import { EnvironmentId, UploadError, WS_METHODS } from "@coddy/contracts";
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
import { stageTerminalImagePaste } from "./terminal.ts";

const IMAGE = new Uint8Array([137, 80, 78, 71]);

function session(commit: () => Effect.Effect<{ path: string }, UploadError>) {
  const stageImagePaste = vi.fn(commit);
  const cancel = vi.fn(() => Effect.succeed({}));
  const rpc: RpcSession = {
    client: {
      [WS_METHODS.uploadsBegin]: () => Effect.succeed({ uploadId: "stage", exists: false }),
      [WS_METHODS.uploadsAppend]: () => Effect.succeed({ receivedBytes: IMAGE.length }),
      [WS_METHODS.uploadsGet]: () =>
        Effect.succeed({
          uploadId: "stage",
          receivedBytes: IMAGE.length,
          sizeBytes: IMAGE.length,
          complete: true,
        }),
      [WS_METHODS.uploadsCancel]: cancel,
      [WS_METHODS.terminalStageImagePaste]: stageImagePaste,
    } as unknown as WsRpcProtocolClient,
    initialConfig: Effect.succeed({
      environment: { capabilities: { attachmentStaging: true } },
    } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
  return { rpc, stageImagePaste, cancel };
}

const supervisorFor = Effect.fn(function* (rpc: RpcSession) {
  const target = { environmentId: EnvironmentId.make("host"), label: "Build server" };
  return EnvironmentSupervisor.of({
    target,
    session: yield* SubscriptionRef.make(Option.some(rpc)),
    state: yield* SubscriptionRef.make<SupervisorConnectionState>({
      ...AVAILABLE_CONNECTION_STATE,
      desired: true,
      phase: "connected",
    }),
    prepared: { descriptor: { capabilities: { attachmentStaging: true } } },
  } as never);
});

/** The command starts on `started`; by commit time the registry has replaced it with `current`. */
const paste = Effect.fn(function* (started: RpcSession, current: RpcSession) {
  const startedSupervisor = yield* supervisorFor(started);
  const currentSupervisor = yield* supervisorFor(current);
  const registry = EnvironmentRegistry.of({
    entries: yield* SubscriptionRef.make(
      new Map([[startedSupervisor.target.environmentId, { target: startedSupervisor.target }]]),
    ),
    followStream: (_id: string, stream: Stream.Stream<unknown, unknown, EnvironmentSupervisor>) =>
      Stream.provideService(stream, EnvironmentSupervisor, startedSupervisor),
    run: <A, E>(_id: string, effect: Effect.Effect<A, E, EnvironmentSupervisor>) =>
      Effect.provideService(effect, EnvironmentSupervisor, currentSupervisor),
  } as never);
  return yield* Effect.exit(
    stageTerminalImagePaste({
      file: new Blob([IMAGE], { type: "image/png" }),
      name: "image.png",
      mimeType: "image/png",
    }).pipe(
      Effect.provideService(EnvironmentRegistry, registry),
      Effect.provideService(EnvironmentSupervisor, startedSupervisor),
    ),
  );
});

describe("terminal image paste", () => {
  it.effect("commits the staged image through the environment's current connection", () =>
    Effect.gen(function* () {
      const started = session(() => Effect.succeed({ path: "/stale.png" }));
      const current = session(() => Effect.succeed({ path: "/state/terminal-pastes/a.png" }));

      const exit = yield* paste(started.rpc, current.rpc);

      expect(exit).toStrictEqual(Exit.succeed("/state/terminal-pastes/a.png"));
      expect(started.stageImagePaste).not.toHaveBeenCalled();
      expect(current.stageImagePaste).toHaveBeenCalledWith({
        uploadId: "stage",
        name: "image.png",
        mimeType: "image/png",
        sizeBytes: IMAGE.length,
      });
      expect(current.cancel).not.toHaveBeenCalled();
    }),
  );

  it.effect("releases the staged image when the commit fails", () =>
    Effect.gen(function* () {
      const started = session(() => Effect.succeed({ path: "/unused.png" }));
      const current = session(() =>
        Effect.fail(new UploadError({ reason: "invalid", message: "disk full" })),
      );

      const exit = yield* paste(started.rpc, current.rpc);

      expect(Exit.isFailure(exit)).toBe(true);
      expect(current.cancel).toHaveBeenCalledWith({ uploadId: "stage" });
    }),
  );
});
