// @effect-diagnostics nodeBuiltinImport:off - The generated qualification handoff runs in an inert VM.
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeVM from "node:vm";
import * as NodeHttp from "node:http";
import * as NodeFS from "node:fs";
import * as NodeOS from "node:os";
import * as NodePath from "node:path";
import type * as NodeNet from "node:net";
import { it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import * as FileSystem from "effect/FileSystem";
import * as Path from "effect/Path";
import * as Schema from "effect/Schema";
import { afterEach, beforeEach, describe, expect, vi } from "vite-plus/test";
import type { RemoteUpdateSnapshot } from "@coddy/contracts";
import {
  assertWebDriverPhaseExit,
  createSeededUpgradeDriverSpec,
} from "../seeded-desktop-upgrade-smoke.ts";
import {
  decodeExit,
  encodeRequest,
  identityFromConfig,
  runRemoteInstallDriver,
} from "./remote-install-driver.ts";

const decodeFixtureJson = Schema.decodeUnknownEffect(Schema.fromJsonString(Schema.Unknown));
const encodeFixtureJson = Schema.encodeSync(Schema.fromJsonString(Schema.Unknown));
const readAuthFixture = Effect.fn("RemoteInstallTest.readAuthFixture")(function* (name: string) {
  const fs = yield* FileSystem.FileSystem;
  const path = yield* Path.Path;
  const filename = yield* path.fromFileUrl(
    new URL(`../../packages/contracts/fixtures/auth-http/responses/${name}.json`, import.meta.url),
  );
  return yield* decodeFixtureJson(yield* fs.readFileString(filename));
}, Effect.provide(NodeServices.layer));

async function ownedUploadServer() {
  const root = await NodeFS.promises.mkdtemp(
    NodePath.join(NodeOS.tmpdir(), "remote-install-http-"),
  );
  const workspaceRoot = NodePath.join(root, "workspace");
  await NodeFS.promises.mkdir(workspaceRoot);
  const headers = Promise.withResolvers<void>();
  const prefix = Promise.withResolvers<void>();
  const completed = Promise.withResolvers<void>();
  const sockets = new Set<NodeNet.Socket>();
  const joins: Promise<void>[] = [];
  const writes: Promise<void>[] = [];
  const state = {
    root,
    workspaceRoot,
    uploadReceiptPath: NodePath.join(root, "upload.secret.json"),
    endpoint: "",
    headers,
    prefix,
    completed,
    requests: 0,
    receiptBeforeHttp: false,
    received: Buffer.alloc(0),
    allowContinue: true,
    continueBody: () => {},
    mode: "success" as "success" | "refuse" | "drop-response" | "wrong-bytes",
    onCompleted: () => {},
  };
  const server = NodeHttp.createServer();
  server.on("connection", (socket) => {
    sockets.add(socket);
    const closed = Promise.withResolvers<void>();
    joins.push(closed.promise);
    socket.once("close", () => {
      sockets.delete(socket);
      closed.resolve();
    });
  });
  server.on("checkContinue", (request, response) => {
    state.requests++;
    request.on("error", () => undefined);
    response.on("error", () => undefined);
    const work = (async () => {
      const receipt = JSON.parse(
        await NodeFS.promises.readFile(state.uploadReceiptPath, "utf8"),
      ) as { relativeDirectory: string; relativeUrl: string };
      state.receiptBeforeHttp = receipt.relativeUrl === request.url;
      let continued = false;
      state.continueBody = () => {
        if (continued || response.headersSent || response.destroyed) return;
        continued = true;
        response.writeContinue();
      };
      headers.resolve();
      if (state.mode === "refuse") {
        response.writeHead(403);
        response.end();
        return;
      }
      request.on("data", (chunk: Buffer) => {
        state.received = Buffer.concat([state.received, chunk]);
        prefix.resolve();
      });
      request.once("end", () => {
        const write = (async () => {
          await NodeFS.promises.writeFile(
            NodePath.join(workspaceRoot, receipt.relativeDirectory, "protection-witness.txt"),
            state.mode === "wrong-bytes" ? "no" : state.received,
          );
          if (state.mode === "drop-response") request.socket.destroy();
          else {
            response.writeHead(201, { "content-type": "application/json" });
            response.end(JSON.stringify({ relativePath: "protection-witness.txt" }));
          }
          state.onCompleted();
          completed.resolve();
        })();
        writes.push(write);
        void write.catch(() => request.socket.destroy());
      });
      if (state.allowContinue) state.continueBody();
    })();
    writes.push(work);
    void work.catch(() => request.socket.destroy());
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  if (address === null || typeof address === "string")
    throw new Error("Owned HTTP fixture address");
  state.endpoint = "http://127.0.0.1:" + address.port;
  return {
    ...state,
    state,
    close: async () => {
      for (const socket of sockets) socket.destroy();
      await Promise.allSettled(writes);
      await new Promise<void>((resolve, reject) =>
        server.close((error) => (error ? reject(error) : resolve())),
      );
      await Promise.all(joins);
      await NodeFS.promises.rm(root, { recursive: true, force: true });
    },
  };
}
let owned: Awaited<ReturnType<typeof ownedUploadServer>>;
const uploadInput = () => ({
  endpoint: owned.state.endpoint,
  workspaceRoot: owned.state.workspaceRoot,
  uploadReceiptPath: owned.state.uploadReceiptPath,
});

const authenticationFixture = (ticket: unknown, token: unknown) => {
  vi.stubGlobal("fetch", async (input: URL) => {
    if (input.pathname === "/oauth/token") return Response.json(token);
    if (input.pathname === "/api/auth/websocket-ticket") return Response.json(ticket);
    throw new Error("Unexpected fixture endpoint.");
  });
  const connected = vi.fn<(url: URL) => void>();
  vi.stubGlobal(
    "WebSocket",
    class {
      static readonly OPEN = 1;
      constructor(url: URL) {
        connected(url);
        // Stop at the real driver's next boundary: never start a socket or coordinator.
        throw new Error("Reached fixture socket boundary.");
      }
    },
  );
  return connected;
};

const terminalFixture = (input: {
  readonly ticket: unknown;
  readonly token: unknown;
  readonly runningVersion: string;
  readonly beforeVersion?: string;
  readonly candidateVersion?: string;
  readonly hostError?: string;
  readonly checkSnapshots?: ReadonlyArray<Partial<RemoteUpdateSnapshot>>;
  readonly disconnectDuringCheck?: boolean;
  readonly holdStatusAfter?: number;
  readonly rejectInstallDispatch?: boolean;
  readonly holdInstallReply?: boolean;
  readonly holdVerification?: boolean;
  readonly installStage?: string;
  readonly maxUploadBytes?: number;
}) => {
  authenticationFixture(input.ticket, input.token);
  let opened = 0;
  let checked = false;
  let installed = false;
  let checkReads = 0;
  let availableVersion: string | null = null;
  const requests: string[] = [];
  const installSnapshots: RemoteUpdateSnapshot[] = [];
  const closed = vi.fn();
  const installRequested = Promise.withResolvers<void>();
  const waitingObserved = Promise.withResolvers<void>();
  const verificationRequested = Promise.withResolvers<void>();
  let releaseInstall = () => {};
  let releaseVerification = () => {};
  let installHeld = input.holdInstallReply === true;
  let verificationHeld = input.holdVerification === true;
  vi.stubGlobal(
    "WebSocket",
    class extends EventTarget {
      static readonly OPEN = 1;
      readyState = 1;
      readonly boot = ++opened;
      constructor(_url: URL) {
        super();
        queueMicrotask(() => this.dispatchEvent(new Event("open")));
      }
      close() {
        if (this.readyState === 3) return;
        this.readyState = 3;
        closed(this.boot);
        this.dispatchEvent(new Event("close"));
      }
      send(raw: string) {
        const request = JSON.parse(raw) as {
          _tag: string;
          tag: string;
          id: string;
          payload?: unknown;
        };
        if (request._tag !== "Request") return;
        if (request.tag === "updater.install" && input.rejectInstallDispatch)
          throw new Error("Fixture install send was rejected.");
        requests.push(request.tag);
        const serverVersion =
          this.boot === 1 ? (input.beforeVersion ?? input.runningVersion) : input.runningVersion;
        const snapshot: RemoteUpdateSnapshot = {
          state: input.hostError ? "error" : availableVersion === null ? "idle" : "installing",
          error: input.hostError ?? null,
          serverVersion,
          latestVersion: availableVersion,
          targetVersion: availableVersion,
          support: { installMode: "interactive", reason: "available", installKind: "unknown" },
          downloadPercent: null,
          installStage: input.installStage ?? "waiting-for-mutations",
        };
        let value: unknown;
        switch (request.tag) {
          case "server.getConfig":
            value = {
              environment: {
                bootId: `sentinel-private-boot-${this.boot}`,
                storageInstanceId: "sentinel-private-storage",
                serverVersion,
                capabilities: { remoteUpdateProgress: true },
              },
            };
            break;
          case "updater.activeWork":
            value = { runningTurns: 0, liveTerminals: 0, queuedMessages: 0 };
            break;
          case "projects.createUploadUrl":
            value = {
              relativeUrl: "/api/transfers/fixture-private-upload",
              expiresAt: 1,
              maxBytes: input.maxUploadBytes ?? 1024,
            };
            break;
          case "updater.check":
          case "updater.status": {
            if (request.tag === "updater.check") {
              checked = true;
              if (input.disconnectDuringCheck) {
                queueMicrotask(() => this.close());
                return;
              }
            }
            if (!checked || installed) {
              if (installed && request.tag === "updater.status") waitingObserved.resolve();
              value = snapshot;
              break;
            }
            if (input.holdStatusAfter !== undefined && checkReads >= input.holdStatusAfter) return;
            const candidateVersion = input.candidateVersion ?? "0.7.3-upgrade.49";
            const overrides =
              input.checkSnapshots?.[Math.min(checkReads++, input.checkSnapshots.length - 1)];
            const check = {
              ...snapshot,
              state: "update-available" as const,
              error: null,
              latestVersion: candidateVersion,
              targetVersion: candidateVersion,
              installStage: null,
              ...overrides,
            };
            if (check.state === "update-available") {
              availableVersion = check.targetVersion ?? check.latestVersion;
            }
            value = check;
            break;
          }
          case "updater.install":
            installed = true;
            installRequested.resolve();
            if (!input.hostError) owned.state.onCompleted = () => this.close();
            installSnapshots.push(snapshot);
            value = snapshot;
            break;
          default:
            throw new Error("Unexpected fixture RPC.");
        }
        const reply = () =>
          queueMicrotask(() => {
            this.dispatchEvent(
              new MessageEvent("message", {
                data: JSON.stringify({
                  _tag: "Exit",
                  requestId: request.id,
                  exit: { _tag: "Success", value },
                }),
              }),
            );
          });
        if (request.tag === "updater.install" && installHeld) releaseInstall = reply;
        else if (request.tag === "server.getConfig" && this.boot > 1 && verificationHeld) {
          verificationRequested.resolve();
          releaseVerification = reply;
        } else reply();
      }
    },
  );
  return {
    requests,
    closed,
    installSnapshots,
    installRequested,
    waitingObserved,
    verificationRequested,
    releaseInstall: () => {
      installHeld = false;
      releaseInstall();
    },
    releaseVerification: () => {
      verificationHeld = false;
      releaseVerification();
    },
  };
};

/** Runs the actual generated marker handoff with the real driver and inert transport. */
const generatedInstallFixture = (failMarkerWrite = false) => {
  const input = {
    candidateVersion: "0.7.3-upgrade.51",
    expectedDataRoot: "/fixture/data",
    lane: "remote-install" as const,
    phase: "seed-and-install" as const,
    projectId: "fixture-project",
    resultPath: "/fixture/before.json",
    workspaceRoot: owned.state.workspaceRoot,
    platform: "linux" as const,
    appBinaryPath: "/fixture/RemoteLane.AppImage",
    remoteInstallDriverPath: "fixture:driver",
    remoteHarnessPath: "fixture:host",
    remoteSecretPath: "/fixture/private.json",
    remoteUploadSecretPath: owned.state.uploadReceiptPath,
    remoteEvidencePath: "/fixture/evidence.json",
  };
  const spec = createSeededUpgradeDriverSpec(input);
  const start = spec.indexOf(
    "    NodeFS.writeFileSync(input.resultPath, JSON.stringify({ ...observation, installAttempted:",
  );
  const end = spec.indexOf("\n  });\n});", start);
  if (start < 0 || end < 0) throw new Error("The generated install handoff is missing.");
  const source = spec.slice(start, end).replaceAll("await import(", "await loadFixture(");
  const markers: boolean[] = [];
  const attempts: boolean[] = [];
  const context = {
    input,
    credentials: { endpoint: owned.state.endpoint, bootstrapToken: "fixture-sharing-grant" },
    observation: { projectId: "fixture-project" },
    widened: false,
    NodeFS: {
      writeFileSync: (path: string, contents: string) => {
        if (path !== input.resultPath) return;
        const value = JSON.parse(contents) as { installAttempted: boolean };
        attempts.push(value.installAttempted);
        if (value.installAttempted && failMarkerWrite)
          throw new Error("Fixture marker write failed.");
        markers.push(value.installAttempted);
      },
    },
    loadFixture: async (path: string) => {
      if (path === input.remoteInstallDriverPath) return { runRemoteInstallDriver };
      if (path === input.remoteHarnessPath)
        return { captureRemoteInstallHostEvidence: async () => ({ requesterLogLines: 1 }) };
      throw new Error("Unexpected fixture import.");
    },
  };
  return {
    markers,
    attempts,
    run: () =>
      NodeVM.runInNewContext(`(async () => { ${source} })()`, context, {
        timeout: 1_000,
      }) as Promise<void>,
  };
};

describe("remote install RPC driver", () => {
  beforeEach(async () => {
    owned = await ownedUploadServer();
  });
  afterEach(async () => {
    vi.useRealTimers();
    await owned.close();
    vi.unstubAllGlobals();
  });

  it.effect(
    "refuses a witness larger than the advertised upload capacity before HTTP or install",
    () =>
      Effect.gen(function* () {
        const fixture = terminalFixture({
          ticket: yield* readAuthFixture("websocket-ticket"),
          token: yield* readAuthFixture("token"),
          beforeVersion: "0.7.2",
          runningVersion: "0.7.3-upgrade.49",
          maxUploadBytes: 1,
        });
        const error = yield* Effect.promise(() =>
          runRemoteInstallDriver({
            ...uploadInput(),
            bootstrapToken: "fixture-grant",
            candidateVersion: "0.7.3-upgrade.49",
          }).catch((error: unknown) => error),
        );
        expect(error).toBeInstanceOf(Error);
        expect(owned.state.requests).toBe(0);
        expect(fixture.requests).not.toContain("updater.install");
      }),
  );
  it.effect("omits invalid endpoint input from its public error", () =>
    Effect.gen(function* () {
      const error = yield* Effect.promise(() =>
        runRemoteInstallDriver({
          ...uploadInput(),
          endpoint: "http://[private-endpoint-input",
          bootstrapToken: "fixture-grant",
          candidateVersion: "0.7.3-upgrade.49",
        }).catch((error: unknown) => error),
      );
      expect(error).toBeInstanceOf(Error);
      expect(String(error) + encodeFixtureJson(error)).not.toContain("private-endpoint-input");
      expect(owned.state.requests).toBe(0);
    }),
  );

  it.effect(
    "holds the real HTTP body until the product coordinator observes the waiting stage",
    () =>
      Effect.gen(function* () {
        const fixture = terminalFixture({
          ticket: yield* readAuthFixture("websocket-ticket"),
          token: yield* readAuthFixture("token"),
          beforeVersion: "0.7.2",
          runningVersion: "0.7.3-upgrade.49",
          holdInstallReply: true,
        });
        owned.state.allowContinue = false;
        const running = runRemoteInstallDriver({
          ...uploadInput(),
          bootstrapToken: "fixture-grant",
          candidateVersion: "0.7.3-upgrade.49",
        });
        try {
          const httpFirst = yield* Effect.promise(() =>
            Promise.race([
              owned.state.headers.promise.then(() => true),
              fixture.installRequested.promise.then(() => false),
              running.then(
                () => false,
                () => false,
              ),
            ]),
          );
          expect(httpFirst).toBe(true);
          expect(owned.state.receiptBeforeHttp).toBe(true);
          expect(fixture.requests).not.toContain("updater.install");
          owned.state.continueBody();
          yield* Effect.promise(() => owned.state.prefix.promise);
          yield* Effect.promise(() => fixture.waitingObserved.promise);
          expect(owned.state.received.toString()).toBe("o");
          fixture.releaseInstall();
          const result = yield* Effect.promise(() => running);
          expect(owned.state.received.toString()).toBe("ok");
          expect(result).toMatchObject({
            heldUpload: {
              admitted: true,
              releasedOnWaitingStage: true,
              completed: true,
              bytesMatch: true,
              noPartials: true,
              requestClosed: true,
            },
          });
        } finally {
          owned.state.continueBody();
          fixture.releaseInstall();
          owned.state.onCompleted();
          yield* Effect.promise(() => running.catch(() => undefined));
        }
      }),
  );

  it.effect("joins coordinator verification after an installed upload response fails", () =>
    Effect.gen(function* () {
      const fixture = terminalFixture({
        ticket: yield* readAuthFixture("websocket-ticket"),
        token: yield* readAuthFixture("token"),
        beforeVersion: "0.7.2",
        runningVersion: "0.7.3-upgrade.49",
        holdVerification: true,
      });
      owned.state.mode = "drop-response";
      let settled = false;
      const running = runRemoteInstallDriver({
        ...uploadInput(),
        bootstrapToken: "fixture-grant",
        candidateVersion: "0.7.3-upgrade.49",
      }).then(
        (value) => {
          settled = true;
          return value;
        },
        (error: unknown) => {
          settled = true;
          return error;
        },
      );
      try {
        const began = yield* Effect.promise(() =>
          Promise.race([
            owned.state.headers.promise.then(() => true),
            fixture.installRequested.promise.then(() => false),
            running.then(() => false),
          ]),
        );
        expect(began).toBe(true);
        yield* Effect.promise(() => owned.state.completed.promise);
        yield* Effect.promise(() => fixture.verificationRequested.promise);
        expect(settled).toBe(false);
        fixture.releaseVerification();
        const error = yield* Effect.promise(() => running);
        expect(error).toBeInstanceOf(Error);
        expect(String(error)).toContain("held upload");
        expect(String(error)).not.toContain("fixture-private-upload");
        expect(fixture.closed.mock.calls).toEqual([[1], [2]]);
      } finally {
        fixture.releaseVerification();
        owned.state.onCompleted();
        yield* Effect.promise(() => running);
      }
    }),
  );

  it.effect("refuses an existing capability receipt before HTTP or install dispatch", () =>
    Effect.gen(function* () {
      const fixture = terminalFixture({
        ticket: yield* readAuthFixture("websocket-ticket"),
        token: yield* readAuthFixture("token"),
        beforeVersion: "0.7.2",
        runningVersion: "0.7.3-upgrade.49",
      });
      yield* Effect.promise(() =>
        NodeFS.promises.writeFile(owned.state.uploadReceiptPath, "private-existing-receipt"),
      );
      const running = runRemoteInstallDriver({
        ...uploadInput(),
        bootstrapToken: "fixture-grant",
        candidateVersion: "0.7.3-upgrade.49",
      });
      // An old driver has no held request; release its inert host solely to bound the RED control.
      const dispatched = fixture.installRequested.promise.then(() => owned.state.onCompleted());
      yield* Effect.promise(() => expect(running).rejects.toThrow("private upload receipt"));
      void dispatched;
      expect(owned.state.requests).toBe(0);
      expect(fixture.requests).not.toContain("updater.install");
      expect(
        yield* Effect.promise(() =>
          NodeFS.promises.readFile(owned.state.uploadReceiptPath, "utf8"),
        ),
      ).toBe("private-existing-receipt");
    }),
  );

  for (const mode of ["refuse", "wrong-bytes"] as const) {
    it.effect("fails a real held upload " + mode + " without accepting false qualification", () =>
      Effect.gen(function* () {
        const fixture = terminalFixture({
          ticket: yield* readAuthFixture("websocket-ticket"),
          token: yield* readAuthFixture("token"),
          beforeVersion: "0.7.2",
          runningVersion: "0.7.3-upgrade.49",
        });
        owned.state.mode = mode;
        const error = yield* Effect.promise(() =>
          runRemoteInstallDriver({
            ...uploadInput(),
            bootstrapToken: "fixture-grant",
            candidateVersion: "0.7.3-upgrade.49",
          }).catch((error: unknown) => error),
        );
        expect(error).toBeInstanceOf(Error);
        expect(String(error)).not.toContain("fixture-private-upload");
        expect(owned.state.receiptBeforeHttp).toBe(true);
        if (mode === "refuse") {
          expect(fixture.requests).not.toContain("updater.install");
          expect(fixture.closed.mock.calls).toEqual([[1]]);
        } else {
          expect(fixture.requests).toContain("updater.install");
          expect(fixture.closed.mock.calls).toEqual([[1], [2]]);
          expect(String(error)).toContain("bytes or residue");
        }
        const receipt = (yield* decodeFixtureJson(
          yield* Effect.promise(() =>
            NodeFS.promises.readFile(owned.state.uploadReceiptPath, "utf8"),
          ),
        )) as { relativeDirectory: string };
        expect(
          NodeFS.existsSync(NodePath.join(owned.state.workspaceRoot, receipt.relativeDirectory)),
        ).toBe(true);
      }),
    );
  }

  it.effect(
    "joins a successful update but fails qualification when the real hold budget expires",
    () =>
      Effect.gen(function* () {
        const fixture = terminalFixture({
          ticket: yield* readAuthFixture("websocket-ticket"),
          token: yield* readAuthFixture("token"),
          beforeVersion: "0.7.2",
          runningVersion: "0.7.3-upgrade.49",
          installStage: "installing",
        });
        vi.useFakeTimers();
        let settled = false;
        const running = runRemoteInstallDriver({
          ...uploadInput(),
          bootstrapToken: "fixture-grant",
          candidateVersion: "0.7.3-upgrade.49",
        }).then(
          (result) => {
            settled = true;
            return result;
          },
          (error: unknown) => {
            settled = true;
            return error;
          },
        );
        try {
          yield* Effect.promise(() => owned.state.prefix.promise);
          yield* Effect.promise(() => fixture.installRequested.promise);
          expect(owned.state.received.toString()).toBe("o");
          yield* Effect.promise(() => vi.advanceTimersByTimeAsync(20_001));
          yield* Effect.promise(() => owned.state.completed.promise);
          for (let count = 0; count < 10; count++) {
            if (settled) break;
            yield* Effect.promise(() => vi.advanceTimersByTimeAsync(1_000));
          }
          expect(settled).toBe(true);
          const error = yield* Effect.promise(() => running);
          expect(String(error)).toContain('"reason":"hold-timeout"');
          expect(String(error)).toContain('"coordinatorPhase":"succeeded"');
          expect(owned.state.received.toString()).toBe("ok");
          expect(fixture.closed.mock.calls).toEqual([[1], [2]]);
        } finally {
          yield* Effect.promise(() => vi.advanceTimersByTimeAsync(25_000));
          owned.state.onCompleted();
          yield* Effect.promise(() => running);
          vi.useRealTimers();
        }
      }),
  );

  for (const [name, options] of [
    ["refused precheck", { checkSnapshots: [{ state: "up-to-date" as const }] }],
    ["disconnected precheck", { disconnectDuringCheck: true }],
    ["rejected install dispatch", { rejectInstallDispatch: true }],
  ] as const) {
    it.effect(`leaves the generated install marker false after ${name}`, () =>
      Effect.gen(function* () {
        const transport = terminalFixture({
          ticket: yield* readAuthFixture("websocket-ticket"),
          token: yield* readAuthFixture("token"),
          beforeVersion: "0.7.2",
          runningVersion: "0.7.3-upgrade.51",
          candidateVersion: "0.7.3-upgrade.51",
          ...options,
        });
        const generated = generatedInstallFixture();
        yield* Effect.promise(() => expect(generated.run()).rejects.toThrow());
        expect(generated.markers).toEqual([false]);
        expect(generated.attempts).toEqual([false]);
        expect(() =>
          assertWebDriverPhaseExit({
            exitCode: 1,
            installAttempted: generated.markers.at(-1) === true,
            lane: "remote-install",
            phase: "seed-and-install",
          }),
        ).toThrow("WebDriver phase exited with code 1");
        expect(transport.requests).not.toContain("updater.install");
        expect(transport.closed).toHaveBeenCalledOnce();
      }),
    );
  }

  for (const failMarkerWrite of [false, true]) {
    it.effect(
      `records the actual install dispatch once without changing the updater outcome (${failMarkerWrite ? "failed" : "successful"} marker write)`,
      () =>
        Effect.gen(function* () {
          const transport = terminalFixture({
            ticket: yield* readAuthFixture("websocket-ticket"),
            token: yield* readAuthFixture("token"),
            beforeVersion: "0.7.2",
            runningVersion: "0.7.3-upgrade.51",
            candidateVersion: "0.7.3-upgrade.51",
          });
          const generated = generatedInstallFixture(failMarkerWrite);
          yield* Effect.promise(() => generated.run());
          expect(generated.attempts).toEqual([false, true]);
          expect(generated.markers).toEqual(failMarkerWrite ? [false] : [false, true]);
          expect(transport.requests.filter((tag) => tag === "updater.install")).toHaveLength(1);
          expect(transport.closed.mock.calls).toEqual([[1], [2]]);
        }),
    );
  }

  it.effect("checks the cold host before a fast candidate restart can hide its target", () =>
    Effect.gen(function* () {
      const fixture = terminalFixture({
        ticket: yield* readAuthFixture("websocket-ticket"),
        token: yield* readAuthFixture("token"),
        beforeVersion: "0.7.2",
        runningVersion: "0.7.3-upgrade.51",
        candidateVersion: "0.7.3-upgrade.51",
      });
      const result = yield* Effect.promise(() =>
        runRemoteInstallDriver({
          ...uploadInput(),
          bootstrapToken: "fixture-sharing-grant",
          candidateVersion: "0.7.3-upgrade.51",
        }),
      );
      expect(result.before.serverVersion).toBe("0.7.2");
      expect(result.after.serverVersion).toBe("0.7.3-upgrade.51");
      expect(result.phases).toEqual([
        "starting",
        "installing",
        "restarting",
        "verifying",
        "succeeded",
      ]);
      expect(fixture.requests.indexOf("updater.check")).toBeLessThan(
        fixture.requests.indexOf("updater.install"),
      );
      expect(fixture.installSnapshots[0]?.targetVersion).toBe("0.7.3-upgrade.51");
      expect(fixture.closed.mock.calls).toEqual([[1], [2]]);
    }),
  );

  it.effect("waits for an already running check to publish the candidate before installing", () =>
    Effect.gen(function* () {
      const fixture = terminalFixture({
        ticket: yield* readAuthFixture("websocket-ticket"),
        token: yield* readAuthFixture("token"),
        beforeVersion: "0.7.2",
        runningVersion: "0.7.3-upgrade.49",
        checkSnapshots: [
          { state: "checking", latestVersion: null, targetVersion: null },
          { state: "checking", latestVersion: null, targetVersion: null },
          {},
        ],
      });
      const result = yield* Effect.promise(() =>
        runRemoteInstallDriver({
          ...uploadInput(),
          bootstrapToken: "fixture-sharing-grant",
          candidateVersion: "0.7.3-upgrade.49",
        }),
      );
      expect(result.phases.at(-1)).toBe("succeeded");
      const beforeInstall = fixture.requests.slice(0, fixture.requests.indexOf("updater.install"));
      expect(beforeInstall.filter((tag) => tag === "updater.check")).toHaveLength(1);
      expect(beforeInstall.filter((tag) => tag === "updater.status").length).toBeGreaterThanOrEqual(
        2,
      );
    }),
  );

  for (const [name, snapshot] of [
    ["host error", { state: "error", error: "sentinel-private-host-error" }],
    ["up to date", { state: "up-to-date" }],
    ["unchecked", { state: "idle", latestVersion: null, targetVersion: null }],
    ["missing target", { latestVersion: null, targetVersion: null }],
    ["wrong target", { latestVersion: "0.7.3-upgrade.50", targetVersion: "0.7.3-upgrade.50" }],
    ["conflicting target", { targetVersion: "0.7.3-upgrade.50" }],
    ["invalid snapshot", { serverVersion: "" }],
    [
      "manual host",
      {
        support: {
          installMode: "manual",
          reason: "manual-update-required",
          installKind: "unknown",
        },
      },
    ],
  ] satisfies ReadonlyArray<readonly [string, Partial<RemoteUpdateSnapshot>]>) {
    it.effect(`refuses ${name} before requesting installation`, () =>
      Effect.gen(function* () {
        const fixture = terminalFixture({
          ticket: yield* readAuthFixture("websocket-ticket"),
          token: yield* readAuthFixture("token"),
          runningVersion: "0.7.2",
          checkSnapshots: [snapshot],
        });
        yield* Effect.promise(() =>
          expect(
            runRemoteInstallDriver({
              ...uploadInput(),
              bootstrapToken: "fixture-sharing-grant",
              candidateVersion: "0.7.3-upgrade.49",
            }),
          ).rejects.toThrow("The candidate update was not available before remote installation."),
        );
        expect(fixture.requests).not.toContain("updater.install");
        expect(fixture.closed).toHaveBeenCalledOnce();
      }),
    );
  }

  it.effect("stops after a disconnected availability check without installing", () =>
    Effect.gen(function* () {
      const fixture = terminalFixture({
        ticket: yield* readAuthFixture("websocket-ticket"),
        token: yield* readAuthFixture("token"),
        runningVersion: "0.7.2",
        disconnectDuringCheck: true,
      });
      yield* Effect.promise(() =>
        expect(
          runRemoteInstallDriver({
            ...uploadInput(),
            bootstrapToken: "fixture-sharing-grant",
            candidateVersion: "0.7.3-upgrade.49",
          }),
        ).rejects.toThrow("The candidate update was not available before remote installation."),
      );
      expect(fixture.requests).not.toContain("updater.install");
      expect(fixture.closed).toHaveBeenCalledOnce();
    }),
  );

  for (const holdStatusAfter of [undefined, 110]) {
    it.effect(
      `bounds the entire availability check and releases pending reads (${holdStatusAfter ?? "checking"})`,
      () =>
        Effect.gen(function* () {
          const fixture = terminalFixture({
            ticket: yield* readAuthFixture("websocket-ticket"),
            token: yield* readAuthFixture("token"),
            runningVersion: "0.7.2",
            checkSnapshots: [{ state: "checking", latestVersion: null, targetVersion: null }],
            ...(holdStatusAfter === undefined ? {} : { holdStatusAfter }),
          });
          yield* Effect.promise(async () => {
            vi.useFakeTimers();
            const result = runRemoteInstallDriver({
              ...uploadInput(),
              bootstrapToken: "fixture-sharing-grant",
              candidateVersion: "0.7.3-upgrade.49",
            }).catch((error: unknown) => error);
            await vi.advanceTimersByTimeAsync(30_001);
            expect(await result).toEqual(
              new Error("The candidate update was not available before remote installation."),
            );
            expect(fixture.requests).not.toContain("updater.install");
            expect(fixture.closed).toHaveBeenCalledOnce();
            const requestCount = fixture.requests.length;
            await vi.advanceTimersByTimeAsync(10_000);
            expect(fixture.requests).toHaveLength(requestCount);
          });
        }),
    );
  }

  it.effect(
    "decodes the public HTTP ticket fixture before opening its own authenticated socket",
    () =>
      Effect.gen(function* () {
        const ticketFixture = (yield* readAuthFixture("websocket-ticket")) as { ticket: string };
        const connected = authenticationFixture(ticketFixture, yield* readAuthFixture("token"));
        yield* Effect.promise(() =>
          expect(
            runRemoteInstallDriver({
              ...uploadInput(),
              bootstrapToken: "fixture-sharing-grant",
              candidateVersion: "0.7.3-upgrade.49",
            }),
          ).rejects.toThrow("Reached fixture socket boundary."),
        );
        expect(connected).toHaveBeenCalledOnce();
        const url = connected.mock.calls[0]![0];
        expect(url.origin).toBe(owned.state.endpoint.replace("http:", "ws:"));
        expect(url.pathname).toBe("/ws");
        expect(url.searchParams.get("wsTicket")).toBe(ticketFixture.ticket);
      }),
  );

  it.effect("rejects a malformed HTTP ticket date without retaining response details", () =>
    Effect.gen(function* () {
      const ticketFixture = (yield* readAuthFixture("websocket-ticket")) as { ticket: string };
      const connected = authenticationFixture(
        {
          ...ticketFixture,
          expiresAt: "fixture-private-date-detail",
        },
        yield* readAuthFixture("token"),
      );
      yield* Effect.promise(() =>
        expect(
          runRemoteInstallDriver({
            ...uploadInput(),
            bootstrapToken: "fixture-sharing-grant",
            candidateVersion: "0.7.3-upgrade.49",
          }),
        ).rejects.toThrow("Invalid verification ticket response."),
      );
      expect(connected).not.toHaveBeenCalled();
    }),
  );

  it.effect("retains the real coordinator's wrong-version verdict after a distinct boot", () =>
    Effect.gen(function* () {
      const fixture = terminalFixture({
        ticket: yield* readAuthFixture("websocket-ticket"),
        token: yield* readAuthFixture("token"),
        runningVersion: "0.7.2",
      });
      const failure = yield* Effect.promise(async () => {
        try {
          await runRemoteInstallDriver({
            ...uploadInput(),
            bootstrapToken: "sentinel-private-grant",
            candidateVersion: "0.7.3-upgrade.49",
          });
        } catch (error) {
          return error;
        }
        throw new Error("Expected remote verification to fail.");
      });
      expect(failure).toBeInstanceOf(Error);
      const message = (failure as Error).message;
      expect(message).toContain('"phase":"failed"');
      expect(message).toContain('"failureKind":"wrong-version"');
      expect(message).toContain('"runningVersion":"0.7.2"');
      expect(message).toContain('"targetVersion":"0.7.3-upgrade.49"');
      expect(message).toContain(
        '"phases":["starting","installing","restarting","verifying","failed"]',
      );
      expect(message).not.toContain("sentinel-private");
      expect(message.length).toBeLessThan(1024);
      expect(fixture.requests).toContain("updater.install");
      expect(fixture.closed.mock.calls).toEqual([[1], [2]]);
    }),
  );

  for (const runningVersion of ["0.7.2", "sentinel-private-version", `0.7.2-${"x".repeat(128)}`]) {
    it.effect(
      `omits host error details and bounds version evidence (${runningVersion.length})`,
      () =>
        Effect.gen(function* () {
          const fixture = terminalFixture({
            ticket: yield* readAuthFixture("websocket-ticket"),
            token: yield* readAuthFixture("token"),
            runningVersion,
            hostError: "sentinel-private-auth grant=secret bearer=secret",
          });
          const failure = yield* Effect.promise(async () => {
            try {
              await runRemoteInstallDriver({
                ...uploadInput(),
                bootstrapToken: "sentinel-private-grant",
                candidateVersion: "0.7.3-upgrade.49",
              });
            } catch (error) {
              return error;
            }
            throw new Error("Expected remote verification to fail.");
          });
          const message = (failure as Error).message;
          expect(message).toContain('"failureKind":"host-error"');
          expect(message).toContain(
            `"runningVersion":${runningVersion === "0.7.2" ? '"0.7.2"' : "null"}`,
          );
          expect(message).toContain('"phases":["starting","failed"]');
          expect(message).not.toMatch(/sentinel-private|secret|bearer|grant/);
          expect(message).not.toContain("x".repeat(128));
          expect(message.length).toBeLessThan(1024);
          expect(fixture.closed).toHaveBeenCalledOnce();
        }),
    );
  }

  it("encodes the real request envelope and reads only a matching successful exit", () => {
    expect(JSON.parse(encodeRequest("7", "updater.status"))).toEqual({
      _tag: "Request",
      id: "7",
      tag: "updater.status",
      payload: {},
      headers: [],
    });
    expect(
      decodeExit(
        { _tag: "Exit", requestId: "7", exit: { _tag: "Success", value: { state: "idle" } } },
        "7",
      ),
    ).toEqual({ state: "idle" });
    expect(
      decodeExit({ _tag: "Exit", requestId: "8", exit: { _tag: "Success", value: {} } }, "7"),
    ).toBeUndefined();
    expect(() =>
      decodeExit({ _tag: "Exit", requestId: "7", exit: { _tag: "Failure", cause: [] } }, "7"),
    ).toThrow();
  });
  it("retains boot/version/capability and supports older descriptors", () => {
    expect(
      identityFromConfig({
        environment: {
          bootId: "b1",
          serverVersion: "0.7.2",
          capabilities: { remoteUpdateProgress: true },
        },
      }),
    ).toEqual({ bootId: "b1", serverVersion: "0.7.2", progress: true });
    expect(
      identityFromConfig({ environment: { serverVersion: "0.6.0", capabilities: {} } }),
    ).toEqual({ bootId: null, serverVersion: "0.6.0", progress: false });
    expect(() => identityFromConfig({ environment: {} })).toThrow();
  });
});
