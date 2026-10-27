import {
  CommandId,
  EnvironmentId,
  MessageId,
  ProviderInstanceId,
  ThreadId,
  ORCHESTRATION_WS_METHODS,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { vi } from "vite-plus/test";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { PrimaryConnectionTarget, type PreparedConnection } from "../connection/model.ts";
import type { RpcSession } from "../rpc/session.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import {
  readAttachmentAdmissionAuthority,
  admitStagedThreadTurn,
} from "./attachmentAdmissionAuthority.ts";
import type { StartThreadTurnInput } from "./commands.ts";

const command: StartThreadTurnInput = {
  commandId: CommandId.make("original"),
  threadId: ThreadId.make("thread"),
  createdAt: "2026-10-02T00:00:00.000Z",
  message: {
    messageId: MessageId.make("original-message"),
    role: "user",
    text: "original",
    attachments: [
      {
        type: "file",
        id: "file",
        name: "a.txt",
        mimeType: "text/plain",
        sizeBytes: 3,
        uploadId: "consumed",
      },
    ],
  },
  modelSelection: { instanceId: ProviderInstanceId.make("codex"), model: "gpt" },
  runtimeMode: "full-access",
  interactionMode: "default",
};
function session(storageInstanceId: string) {
  const dispatch = vi.fn(() => Effect.succeed({ sequence: 1 }));
  const rpc: RpcSession = {
    client: {
      [ORCHESTRATION_WS_METHODS.dispatchCommand]: dispatch,
    } as unknown as WsRpcProtocolClient,
    initialConfig: Effect.succeed({ environment: { storageInstanceId } } as never),
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
  return { rpc, dispatch };
}
const harness = Effect.fn("TestAdmissionAuthority.harness")(function* () {
  const first = session("store");
  const target = new PrimaryConnectionTarget({
    environmentId: EnvironmentId.make("host"),
    label: "Host",
    httpBaseUrl: "https://host.test",
    wsBaseUrl: "wss://host.test",
  });
  const connection = { httpBaseUrl: "https://host.test", e2ee: null } as PreparedConnection;
  const prepared = yield* SubscriptionRef.make(Option.some(connection));
  const sessions = yield* SubscriptionRef.make(Option.some(first.rpc));
  const supervisor = EnvironmentSupervisor.of({ target, session: sessions, prepared } as never);
  const registry = EnvironmentRegistry.of({} as never);
  const provide = <A, E>(
    effect: Effect.Effect<A, E, EnvironmentSupervisor | EnvironmentRegistry>,
  ) =>
    effect.pipe(
      Effect.provideService(EnvironmentSupervisor, supervisor),
      Effect.provideService(EnvironmentRegistry, registry),
    );
  return { first, connection, prepared, sessions, provide };
});
describe("staged admission authority", () => {
  it.effect(
    "keeps a fresh legacy inline send usable when the server has no storage identity or staging capability",
    () =>
      Effect.gen(function* () {
        const h = yield* harness();
        const legacy = {
          ...h.first.rpc,
          initialConfig: Effect.succeed({
            environment: { storageInstanceId: null, capabilities: { attachmentStaging: false } },
          } as never),
        };
        yield* SubscriptionRef.set(h.sessions, Option.some(legacy));
        expect(yield* h.provide(readAttachmentAdmissionAuthority())).toBeNull();
      }),
  );
  it.effect(
    "replays exactly on a current authorized session after credential/session rotation on the same store",
    () =>
      Effect.gen(function* () {
        const h = yield* harness();
        const authority = yield* h.provide(readAttachmentAdmissionAuthority());
        if (authority === null) throw new Error("fixture must identify its store");
        const current = session("store");
        yield* SubscriptionRef.set(h.sessions, Option.some(current.rpc));
        yield* h.provide(admitStagedThreadTurn(command, authority));
        expect(h.first.dispatch).not.toHaveBeenCalled();
        expect(current.dispatch).toHaveBeenCalledExactlyOnceWith({
          ...command,
          type: "thread.turn.start",
        });
      }),
  );
  it.effect("refuses to dispatch immutable intent onto a different store or host", () =>
    Effect.gen(function* () {
      const h = yield* harness();
      const authority = yield* h.provide(readAttachmentAdmissionAuthority());
      if (authority === null) throw new Error("fixture must identify its store");
      const other = session("other-store");
      yield* SubscriptionRef.set(h.sessions, Option.some(other.rpc));
      expect(
        Exit.isFailure(yield* Effect.exit(h.provide(admitStagedThreadTurn(command, authority)))),
      ).toBe(true);
      expect(other.dispatch).not.toHaveBeenCalled();
      yield* SubscriptionRef.set(h.sessions, Option.some(h.first.rpc));
      yield* SubscriptionRef.set(
        h.prepared,
        Option.some({ ...h.connection, httpBaseUrl: "https://other.test" }),
      );
      expect(
        Exit.isFailure(yield* Effect.exit(h.provide(admitStagedThreadTurn(command, authority)))),
      ).toBe(true);
      expect(h.first.dispatch).not.toHaveBeenCalled();
    }),
  );
});
