import {
  CommandId,
  EnvironmentId,
  MessageId,
  ORCHESTRATION_WS_METHODS,
  ProjectId,
  ThreadId,
  type ClientOrchestrationCommand,
} from "@coddy/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as Crypto from "effect/Crypto";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as SubscriptionRef from "effect/SubscriptionRef";

import {
  AVAILABLE_CONNECTION_STATE,
  PrimaryConnectionTarget,
  type PreparedConnection,
} from "../connection/model.ts";
import * as EnvironmentSupervisor from "../connection/supervisor.ts";
import * as RpcSession from "../rpc/session.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import { archiveThread, createProject, stopThreadSession } from "./commands.ts";
import * as EnvironmentCommands from "./commands.ts";

const TEST_CRYPTO_LAYER = Layer.succeed(
  Crypto.Crypto,
  Crypto.make({
    randomBytes: (size) => new Uint8Array(size),
    digest: (_algorithm, data) => Effect.succeed(data),
  }),
);

const TARGET = new PrimaryConnectionTarget({
  environmentId: EnvironmentId.make("environment-1"),
  label: "Test environment",
  httpBaseUrl: "https://environment.example.test",
  wsBaseUrl: "wss://environment.example.test",
});

const makeSupervisor = Effect.fn("TestEnvironmentCommands.makeSupervisor")(function* (
  dispatched: ClientOrchestrationCommand[],
) {
  const client = {
    [ORCHESTRATION_WS_METHODS.dispatchCommand]: (command: ClientOrchestrationCommand) =>
      Effect.sync(() => {
        dispatched.push(command);
        return { sequence: dispatched.length };
      }),
  } as unknown as WsRpcProtocolClient;
  const session: RpcSession.RpcSession = {
    client,
    initialConfig: Effect.never,
    ready: Effect.void,
    probe: Effect.void,
    closed: Effect.never,
    e2eeAuthenticated: Effect.succeed(null),
  };
  return EnvironmentSupervisor.EnvironmentSupervisor.of({
    target: TARGET,
    state: yield* SubscriptionRef.make(AVAILABLE_CONNECTION_STATE),
    session: yield* SubscriptionRef.make(Option.some(session)),
    prepared: yield* SubscriptionRef.make(Option.none<PreparedConnection>()),
    connect: Effect.void,
    disconnect: Effect.void,
    retryNow: Effect.void,
  } satisfies EnvironmentSupervisor.EnvironmentSupervisor["Service"]);
});

describe("environment commands", () => {
  it.effect("dispatches steer and promote with stable command metadata", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);
      for (const [operation, type] of [
        ["steerThreadTurn", "thread.turn.steer"],
        ["promoteThreadTurn", "thread.turn.promote"],
      ] as const) {
        const input = {
          commandId: CommandId.make(type),
          threadId: ThreadId.make("thread-1"),
          messageId: MessageId.make("message-1"),
          createdAt: "2026-09-22T12:00:00.000Z",
        };
        yield* EnvironmentCommands[operation](input).pipe(
          Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        );
        expect(dispatched.at(-1)).toEqual({ ...input, type });
      }
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );

  it.effect("preserves queued start flags and accepts cancel through the typed RPC", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);
      const metadata = {
        threadId: ThreadId.make("thread-1"),
        createdAt: "2026-09-22T12:00:00.000Z",
      };
      for (const flags of [{}, { queued: true }, { queued: false }]) {
        const input = {
          ...metadata,
          ...flags,
          message: {
            messageId: MessageId.make("message-1"),
            role: "user" as const,
            text: "next task",
            attachments: [],
          },
          runtimeMode: "full-access" as const,
          interactionMode: "default" as const,
        };
        yield* EnvironmentCommands.startThreadTurn(input).pipe(
          Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor),
        );
        expect(dispatched.at(-1)).toEqual({
          ...input,
          type: "thread.turn.start",
          commandId: "00000000-0000-4000-8000-000000000000",
        });
      }
      yield* EnvironmentCommands.resolveTurnDelivery({
        ...metadata,
        messageId: MessageId.make("message-1"),
        action: "cancel",
      }).pipe(Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor));
      expect(dispatched.at(-1)).toEqual({
        ...metadata,
        type: "thread.turn-delivery.resolve",
        commandId: "00000000-0000-4000-8000-000000000000",
        messageId: "message-1",
        action: "cancel",
      });
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );

  it.effect("adds generated command metadata", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);

      const result = yield* createProject({
        projectId: ProjectId.make("project-1"),
        title: "Project",
        workspaceRoot: "/workspace/project",
        createdAt: "2026-06-06T00:00:00.000Z",
      }).pipe(Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor));

      expect(result).toEqual({ sequence: 1 });
      expect(dispatched).toEqual([
        {
          type: "project.create",
          commandId: "00000000-0000-4000-8000-000000000000",
          projectId: "project-1",
          title: "Project",
          workspaceRoot: "/workspace/project",
          createdAt: "2026-06-06T00:00:00.000Z",
        },
      ]);
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );

  it.effect("preserves atomic Git project creation options", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);

      yield* createProject({
        commandId: CommandId.make("create-git-project"),
        projectId: ProjectId.make("project-git"),
        title: "Git Project",
        workspaceRoot: "/tmp/project-git",
        createWorkspaceRootIfMissing: true,
        initializeGit: true,
        createdAt: "2026-07-17T00:00:00.000Z",
      }).pipe(Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor));

      expect(dispatched[0]).toMatchObject({
        type: "project.create",
        projectId: "project-git",
        createWorkspaceRootIfMissing: true,
        initializeGit: true,
      });
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );

  it.effect("preserves caller metadata for idempotent queued commands", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);

      yield* stopThreadSession({
        commandId: CommandId.make("queued-command"),
        threadId: ThreadId.make("thread-1"),
        createdAt: "2026-06-06T00:01:00.000Z",
      }).pipe(Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor));

      expect(dispatched).toEqual([
        {
          type: "thread.session.stop",
          commandId: "queued-command",
          threadId: "thread-1",
          createdAt: "2026-06-06T00:01:00.000Z",
        },
      ]);
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );

  it.effect("does not add timestamps to commands without createdAt", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);

      yield* archiveThread({
        commandId: CommandId.make("archive-command"),
        threadId: ThreadId.make("thread-1"),
      }).pipe(Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor));

      expect(dispatched).toEqual([
        {
          type: "thread.archive",
          commandId: "archive-command",
          threadId: "thread-1",
        },
      ]);
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );

  it.effect("dispatches a typed turn delivery resolution with command metadata", () =>
    Effect.gen(function* () {
      const dispatched: ClientOrchestrationCommand[] = [];
      const supervisor = yield* makeSupervisor(dispatched);
      const resolveTurnDelivery = (EnvironmentCommands as unknown as Record<string, unknown>)
        .resolveTurnDelivery;

      expect(resolveTurnDelivery).toBeTypeOf("function");
      yield* (
        resolveTurnDelivery as (input: {
          commandId: CommandId;
          threadId: ThreadId;
          messageId: ReturnType<typeof MessageId.make>;
          action: "retry" | "dismiss";
          createdAt: string;
        }) => ReturnType<typeof stopThreadSession>
      )({
        commandId: CommandId.make("resolve-delivery"),
        threadId: ThreadId.make("thread-1"),
        messageId: MessageId.make("message-1"),
        action: "retry",
        createdAt: "2026-08-03T00:00:00.000Z",
      }).pipe(Effect.provideService(EnvironmentSupervisor.EnvironmentSupervisor, supervisor));

      expect(dispatched).toEqual([
        {
          type: "thread.turn-delivery.resolve",
          commandId: "resolve-delivery",
          threadId: "thread-1",
          messageId: "message-1",
          action: "retry",
          createdAt: "2026-08-03T00:00:00.000Z",
        },
      ]);
    }).pipe(Effect.provide(TEST_CRYPTO_LAYER)),
  );
});
