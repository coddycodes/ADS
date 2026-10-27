// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from "vite-plus/test";

import {
  configureDesktopActivityCodexExecutable,
  materializeDesktopActivitySession,
  startDesktopActivityComposerFollowupTurn,
  startDesktopActivityFollowupTurn,
} from "./activity-session.ts";

const sentRequests: Array<{
  readonly _tag: string;
  readonly id: string;
  readonly tag: string;
  readonly payload: Record<string, unknown>;
}> = [];

class FixtureWebSocket {
  static readonly CLOSED = 3;
  static readonly CLOSING = 2;
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;

  onclose: ((event: CloseEvent) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  onmessage: ((event: MessageEvent) => void) | null = null;
  onopen: ((event: Event) => void) | null = null;
  readyState = FixtureWebSocket.CONNECTING;
  readonly messageListeners = new Set<(event: MessageEvent) => void>();
  readonly closeListeners = new Set<EventListener>();
  readonly errorListeners = new Set<EventListener>();
  readonly openListeners = new Set<EventListener>();
  readonly url: string;
  readonly controlTags: string[] = [];
  closeCalls = 0;
  private pendingReadyPublisher: (() => void) | null = null;
  static instances: FixtureWebSocket[] = [];
  static responseMode:
    | "close"
    | "malformed"
    | "pending-then-success"
    | "server-error"
    | "silent"
    | "success" = "success";

  constructor(url: string) {
    this.url = url;
    FixtureWebSocket.instances.push(this);
    queueMicrotask(() => {
      this.readyState = FixtureWebSocket.OPEN;
      const event = new Event("open");
      this.onopen?.(event);
      for (const listener of this.openListeners) listener(event);
    });
  }

  close(): void {
    this.closeCalls += 1;
    this.readyState = FixtureWebSocket.CLOSED;
    const event = new CloseEvent("close");
    this.onclose?.(event);
    for (const listener of this.closeListeners) listener(event);
  }

  addEventListener(type: string, listener: EventListener): void {
    if (type === "message") {
      this.messageListeners.add(listener as (event: MessageEvent) => void);
    } else if (type === "open") {
      this.openListeners.add(listener);
    } else if (type === "close") {
      this.closeListeners.add(listener);
    } else if (type === "error") {
      this.errorListeners.add(listener);
    }
  }

  removeEventListener(type: string, listener: EventListener): void {
    if (type === "message") {
      this.messageListeners.delete(listener as (event: MessageEvent) => void);
    } else if (type === "open") {
      this.openListeners.delete(listener);
    } else if (type === "close") {
      this.closeListeners.delete(listener);
    } else if (type === "error") {
      this.errorListeners.delete(listener);
    }
  }

  send(encoded: string): void {
    const request = JSON.parse(encoded) as (typeof sentRequests)[number] & {
      readonly requestId?: string;
    };
    if (request._tag === "Eof" || request._tag === "Ack" || request._tag === "Interrupt") {
      this.controlTags.push(request._tag);
      if (request._tag === "Ack" && this.pendingReadyPublisher !== null) {
        const publishReady = this.pendingReadyPublisher;
        this.pendingReadyPublisher = null;
        queueMicrotask(publishReady);
      }
      return;
    }
    sentRequests.push(request);
    if (FixtureWebSocket.responseMode === "silent") return;
    if (FixtureWebSocket.responseMode === "close") {
      queueMicrotask(() => this.close());
      return;
    }
    if (FixtureWebSocket.responseMode === "malformed") {
      queueMicrotask(() => {
        const event = new MessageEvent("message", { data: "{not-json" });
        for (const listener of this.messageListeners) listener(event);
      });
      return;
    }
    if (FixtureWebSocket.responseMode === "server-error") {
      queueMicrotask(() => {
        const event = new MessageEvent("message", {
          data: JSON.stringify({
            _tag: "Exit",
            requestId: request.id,
            exit: { _tag: "Failure", cause: [{ _tag: "Fail", error: "fixture rejected" }] },
          }),
        });
        for (const listener of this.messageListeners) listener(event);
      });
      return;
    }
    if (request.tag === "orchestration.subscribeThread") {
      const publishSnapshot = (providerInstanceId: string | null) => {
        const event = new MessageEvent("message", {
          data: JSON.stringify({
            _tag: "Chunk",
            requestId: request.id,
            values: [
              {
                kind: "snapshot",
                snapshot: {
                  snapshotSequence: 43,
                  thread: {
                    id: "server-resolved-thread",
                    projectId: "server-resolved-project",
                    session:
                      providerInstanceId === null
                        ? null
                        : {
                            activeTurnId: null,
                            lastError: null,
                            providerInstanceId,
                            providerName: "codex",
                            runtimeMode: "full-access",
                            status: "ready",
                            threadId: "server-resolved-thread",
                            updatedAt: "2026-07-29T19:00:00.000Z",
                          },
                  },
                },
              },
            ],
          }),
        });
        for (const listener of this.messageListeners) listener(event);
      };
      queueMicrotask(() => {
        if (FixtureWebSocket.responseMode === "pending-then-success") {
          this.pendingReadyPublisher = () => publishSnapshot("server-resolved-codex");
          publishSnapshot(null);
          return;
        }
        publishSnapshot("server-resolved-codex");
      });
      return;
    }
    const value =
      request.payload.type === "project.create"
        ? { sequence: 41, projectId: "server-resolved-project" }
        : { sequence: 42 };
    queueMicrotask(() => {
      const event = new MessageEvent("message", {
        data: JSON.stringify({
          _tag: "Exit",
          requestId: request.id,
          exit: { _tag: "Success", value },
        }),
      });
      this.onmessage?.(event);
      for (const listener of this.messageListeners) listener(event);
    });
  }
}

afterEach(() => {
  sentRequests.splice(0);
  FixtureWebSocket.instances.splice(0);
  FixtureWebSocket.responseMode = "success";
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function installHarness(
  fetchImplementation: typeof fetch = vi.fn(async () =>
    Response.json({ ticket: "fixture-ticket", expiresAt: "2026-07-29T19:00:00.000Z" }),
  ) as typeof fetch,
): void {
  Object.defineProperty(window, "desktopBridge", {
    configurable: true,
    value: {
      getLocalEnvironmentBearerToken: vi.fn(async () => "fixture-bearer"),
      getLocalEnvironmentBootstraps: () => [
        {
          id: "primary",
          httpBaseUrl: "http://127.0.0.1:3773",
          wsBaseUrl: "ws://127.0.0.1:3773",
        },
      ],
    },
  });
  vi.stubGlobal("fetch", fetchImplementation);
  vi.stubGlobal("WebSocket", FixtureWebSocket);
  vi.stubGlobal("browser", {
    execute: async (
      callback: (...args: ReadonlyArray<unknown>) => unknown,
      ...args: ReadonlyArray<unknown>
    ) => callback(...args),
  });
}

describe("materializeDesktopActivitySession", () => {
  it("uses the packaged bootstrap RPC and threads a deduplicated project id into session creation", async () => {
    Object.defineProperty(window, "desktopBridge", {
      configurable: true,
      value: {
        getLocalEnvironmentBearerToken: vi.fn(async () => "fixture-bearer"),
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            httpBaseUrl: "http://127.0.0.1:3773",
            wsBaseUrl: "ws://127.0.0.1:3773",
          },
        ],
      },
    });
    const fetch = vi.fn(async () =>
      Response.json({ ticket: "fixture-ticket", expiresAt: "2026-07-29T19:00:00.000Z" }),
    );
    vi.stubGlobal("fetch", fetch);
    vi.stubGlobal("WebSocket", FixtureWebSocket);
    vi.stubGlobal("browser", {
      execute: async (
        callback: (...args: ReadonlyArray<unknown>) => unknown,
        ...args: ReadonlyArray<unknown>
      ) => callback(...args),
    });

    const result = await materializeDesktopActivitySession("/fixture/project");

    expect(fetch).toHaveBeenCalledExactlyOnceWith(
      new URL("http://127.0.0.1:3773/api/auth/websocket-ticket"),
      {
        headers: { authorization: "Bearer fixture-bearer" },
        method: "POST",
        signal: expect.any(AbortSignal),
      },
    );
    expect(result).toEqual({
      projectId: "server-resolved-project",
      providerInstanceId: "server-resolved-codex",
      threadId: "server-resolved-thread",
    });
    expect(sentRequests.map(({ tag }) => tag)).toEqual([
      "orchestration.dispatchCommand",
      "orchestration.dispatchCommand",
      "orchestration.dispatchCommand",
      "orchestration.subscribeThread",
    ]);
    expect(sentRequests.slice(0, 3).map(({ payload }) => payload.type)).toEqual([
      "project.create",
      "thread.create",
      "thread.turn.start",
    ]);
    expect(sentRequests[1]?.payload.projectId).toBe("server-resolved-project");
    const socket = FixtureWebSocket.instances[0]!;
    expect(socket.closeCalls).toBe(1);
    expect(socket.messageListeners.size).toBe(0);
    expect(socket.closeListeners.size).toBe(0);
    expect(socket.errorListeners.size).toBe(0);
    expect(socket.openListeners.size).toBe(0);
  });

  it("waits on the retained thread stream until provider ownership is projected", async () => {
    vi.useFakeTimers();
    installHarness();
    FixtureWebSocket.responseMode = "pending-then-success";

    const materialization = materializeDesktopActivitySession("/fixture/project");
    await vi.advanceTimersByTimeAsync(15_000);
    await expect(materialization).resolves.toEqual({
      projectId: "server-resolved-project",
      providerInstanceId: "server-resolved-codex",
      threadId: "server-resolved-thread",
    });

    expect(sentRequests.filter(({ tag }) => tag === "orchestration.subscribeThread")).toHaveLength(
      1,
    );
    expect(FixtureWebSocket.instances[0]?.controlTags).toEqual(["Ack", "Interrupt", "Eof"]);
  });

  it("can publish a later provider update without rebuilding project or thread state", async () => {
    Object.defineProperty(window, "desktopBridge", {
      configurable: true,
      value: {
        getLocalEnvironmentBearerToken: vi.fn(async () => "fixture-bearer"),
        getLocalEnvironmentBootstraps: () => [
          {
            id: "primary",
            httpBaseUrl: "http://127.0.0.1:3773",
            wsBaseUrl: "ws://127.0.0.1:3773",
          },
        ],
      },
    });
    vi.stubGlobal(
      "fetch",
      vi.fn(async () =>
        Response.json({ ticket: "fixture-ticket", expiresAt: "2026-07-29T19:00:00.000Z" }),
      ),
    );
    vi.stubGlobal("WebSocket", FixtureWebSocket);
    vi.stubGlobal("browser", {
      execute: async (
        callback: (...args: ReadonlyArray<unknown>) => unknown,
        ...args: ReadonlyArray<unknown>
      ) => callback(...args),
    });

    await startDesktopActivityFollowupTurn();

    expect(sentRequests).toHaveLength(1);
    expect(sentRequests[0]?.payload).toEqual({
      type: "thread.turn.start",
      commandId: "coddy-studio-ui-activity-followup-turn-start",
      threadId: "coddy-studio-ui-activity-thread",
      message: {
        messageId: "coddy-studio-ui-activity-followup-message",
        role: "user",
        text: "publish deterministic live activity update",
        attachments: [],
      },
      modelSelection: {
        instanceId: "codex",
        model: "gpt-5.4",
        options: [{ id: "reasoningEffort", value: "medium" }],
      },
      runtimeMode: "full-access",
      interactionMode: "default",
      createdAt: expect.any(String),
    });
  });

  it("can publish a distinct composer-focused provider update", async () => {
    installHarness();

    await startDesktopActivityComposerFollowupTurn();

    expect(sentRequests).toHaveLength(1);
    expect(sentRequests[0]?.payload).toEqual(
      expect.objectContaining({
        type: "thread.turn.start",
        commandId: "coddy-studio-ui-activity-composer-followup-turn-start",
        threadId: "coddy-studio-ui-activity-thread",
        message: expect.objectContaining({
          messageId: "coddy-studio-ui-activity-composer-followup-message",
          text: "publish deterministic composer activity update",
        }),
        createdAt: expect.any(String),
      }),
    );
  });

  it("synchronizes the activity terminal with the exact configured Codex executable", async () => {
    installHarness();

    await configureDesktopActivityCodexExecutable("/fixture/provider-shims/codex");

    const setupRequests = sentRequests;
    expect(setupRequests).toContainEqual({
      _tag: "Request",
      id: "0",
      tag: "server.updateSettings",
      payload: {
        patch: expect.objectContaining({
          enableTerminalAgentActivity: true,
        }),
      },
      headers: [],
    });
    expect(sentRequests).toEqual([
      {
        _tag: "Request",
        id: "0",
        tag: "server.updateSettings",
        payload: {
          patch: {
            enableTerminalAgentActivity: true,
            providers: {
              codex: {
                enabled: true,
                binaryPath: "/fixture/provider-shims/codex",
              },
            },
          },
        },
        headers: [],
      },
    ]);
    expect(FixtureWebSocket.instances[0]?.closeCalls).toBe(1);
  });

  it.each([
    ["server-error", "Activity fixture RPC failed"],
    ["malformed", "received malformed JSON"],
    ["close", "closed with requests pending"],
  ] as const)("rejects %s responses and releases every socket listener", async (mode, message) => {
    installHarness();
    FixtureWebSocket.responseMode = mode;

    await expect(materializeDesktopActivitySession("/fixture/project")).rejects.toThrow(message);

    const socket = FixtureWebSocket.instances[0]!;
    expect(socket.closeCalls).toBe(1);
    expect(socket.messageListeners.size).toBe(0);
    expect(socket.closeListeners.size).toBe(0);
    expect(socket.errorListeners.size).toBe(0);
    expect(socket.openListeners.size).toBe(0);
  });

  it("aborts a stalled ticket request at its bounded timeout", async () => {
    vi.useFakeTimers();
    const fetch = vi.fn(
      (_url: URL | RequestInfo, init?: RequestInit) =>
        new Promise<Response>((_resolve, reject) => {
          init?.signal?.addEventListener("abort", () =>
            reject(new DOMException("aborted", "AbortError")),
          );
        }),
    ) as typeof globalThis.fetch;
    installHarness(fetch);

    const materialization = materializeDesktopActivitySession("/fixture/project");
    const rejection = expect(materialization).rejects.toThrow(
      "Timed out requesting the activity fixture WebSocket ticket.",
    );
    await vi.advanceTimersByTimeAsync(15_000);

    await rejection;
    expect(FixtureWebSocket.instances).toHaveLength(0);
  });

  it("times out a stalled RPC, closes its socket, and removes all listeners", async () => {
    vi.useFakeTimers();
    installHarness();
    FixtureWebSocket.responseMode = "silent";

    const materialization = materializeDesktopActivitySession("/fixture/project");
    const rejection = expect(materialization).rejects.toThrow(
      "Timed out waiting for activity fixture RPC orchestration.dispatchCommand.",
    );
    await vi.advanceTimersByTimeAsync(15_000);

    await rejection;
    const socket = FixtureWebSocket.instances[0]!;
    expect(socket.closeCalls).toBe(1);
    expect(socket.messageListeners.size).toBe(0);
    expect(socket.closeListeners.size).toBe(0);
    expect(socket.errorListeners.size).toBe(0);
    expect(socket.openListeners.size).toBe(0);
  });
});
