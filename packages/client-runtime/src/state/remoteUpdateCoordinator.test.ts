import { describe, expect, it } from "@effect/vitest";
import {
  REMOTE_UPDATE_MANUAL_REQUIRED,
  RemoteUpdateInstallError,
  type RemoteUpdateSnapshot,
} from "@coddy/contracts";
import * as Clock from "effect/Clock";
import * as Data from "effect/Data";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Ref from "effect/Ref";
import * as TestClock from "effect/testing/TestClock";

import {
  REMOTE_UPDATE_DOWNLOAD_BUDGET_MS,
  REMOTE_UPDATE_INSTALL_BUDGET_MS,
  REMOTE_UPDATE_INSTALL_TIMEOUT_MS,
  REMOTE_UPDATE_POLL_MS,
  REMOTE_UPDATE_READ_TIMEOUT_MS,
  REMOTE_UPDATE_RESTART_BUDGET_MS,
  REMOTE_UPDATE_RETRY_INTERVAL_MS,
  type RemoteUpdateConnectionView,
  type RemoteUpdatePort,
  type RemoteUpdateRunState,
  type RemoteUpdateServerIdentity,
  isRemoteUpdateRunActive,
  runRemoteUpdate,
} from "./remoteUpdateCoordinator.ts";

const snapshot = (overrides: Partial<RemoteUpdateSnapshot> = {}): RemoteUpdateSnapshot => ({
  serverVersion: "0.6.2",
  latestVersion: "0.6.4",
  state: "update-available",
  error: null,
  support: { installMode: "interactive", reason: "available", installKind: "unknown" },
  downloadPercent: null,
  targetVersion: "0.6.4",
  installStage: null,
  ...overrides,
});

const OLD_BOOT: RemoteUpdateServerIdentity = {
  bootId: "boot-1",
  serverVersion: "0.6.2",
  progress: true,
};

class IdentityReadError extends Data.TaggedError("IdentityReadError") {}

const makeFakeHost = Effect.fn("TestRemoteUpdate.makeFakeHost")(function* (
  options: {
    readonly install?: Effect.Effect<RemoteUpdateSnapshot, RemoteUpdateInstallError>;
    readonly identity?: RemoteUpdateServerIdentity;
    readonly readIdentity?: (
      identity: RemoteUpdateServerIdentity,
      call: number,
    ) => Effect.Effect<RemoteUpdateServerIdentity, IdentityReadError>;
    readonly readStatus?: (snapshot: RemoteUpdateSnapshot) => Effect.Effect<RemoteUpdateSnapshot>;
  } = {},
) {
  const connection = yield* Ref.make<RemoteUpdateConnectionView>({
    phase: "connected" as const,
    connectedEpoch: 1,
    blockedMessage: null,
  });
  const identity = yield* Ref.make<RemoteUpdateServerIdentity>(options.identity ?? OLD_BOOT);
  const status = yield* Ref.make<RemoteUpdateSnapshot>(
    snapshot({ state: "downloading", downloadPercent: 0 }),
  );
  const retries = yield* Ref.make(0);
  const retryTimes = yield* Ref.make<ReadonlyArray<number>>([]);
  const identityCalls = yield* Ref.make(0);
  const published = yield* Ref.make<ReadonlyArray<RemoteUpdateRunState>>([]);
  const port: RemoteUpdatePort = {
    connection: Ref.get(connection),
    identity: Effect.gen(function* () {
      const current = yield* Ref.get(identity);
      const call = yield* Ref.updateAndGet(identityCalls, (count) => count + 1);
      return yield* options.readIdentity?.(current, call) ?? Effect.succeed(current);
    }),
    status: Ref.get(status).pipe(
      Effect.flatMap((current) => options.readStatus?.(current) ?? Effect.succeed(current)),
    ),
    install: options.install ?? Ref.get(status),
    retryNow: Effect.gen(function* () {
      yield* Ref.update(retries, (count) => count + 1);
      const now = yield* Clock.currentTimeMillis;
      yield* Ref.update(retryTimes, (times) => [...times, now]);
    }),
  };
  const run = yield* runRemoteUpdate(port, (state) =>
    Ref.update(published, (states) => [...states, state]),
  ).pipe(Effect.forkChild({ startImmediately: true }));
  const phases = Ref.get(published).pipe(
    Effect.map((states) => states.map((state) => state.phase)),
  );
  /** The host restarts: the socket drops, then a new boot answers. */
  const restart = Effect.fn("TestRemoteUpdate.restart")(function* (
    next: RemoteUpdateServerIdentity,
    after: `${number} seconds`,
  ) {
    yield* Ref.update(connection, (view) => ({ ...view, phase: "backoff" as const }));
    yield* TestClock.adjust(after);
    yield* Ref.set(identity, next);
    yield* Ref.update(connection, (view) => ({
      ...view,
      phase: "connected" as const,
      connectedEpoch: view.connectedEpoch + 1,
    }));
  });
  return { connection, identity, status, retries, retryTimes, published, run, phases, restart };
});

describe("runRemoteUpdate", () => {
  it.effect("follows download, backup and restart to a new boot on the target version", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.set(host.status, snapshot({ state: "downloading", downloadPercent: 42 }));
      yield* TestClock.adjust("1 second");
      yield* Ref.set(
        host.status,
        snapshot({ state: "installing", installStage: "creating-verified-backup" }),
      );
      yield* TestClock.adjust("1 second");
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.4", progress: true },
        "20 seconds",
      );
      yield* TestClock.adjust("1 second");

      const outcome = yield* Fiber.join(host.run);
      expect(outcome).toEqual({ phase: "succeeded", version: "0.6.4" });
      const states = yield* Ref.get(host.published);
      expect(states).toContainEqual({ phase: "downloading", percent: 42, targetVersion: "0.6.4" });
      expect(states).toContainEqual({
        phase: "installing",
        stage: "creating-verified-backup",
        targetVersion: "0.6.4",
      });
      expect(yield* host.phases).toEqual(
        expect.arrayContaining(["starting", "restarting", "verifying", "succeeded"]),
      );
      expect(states.at(-1)).toEqual(outcome);
    }),
  );

  it.effect("ends with the host's error while the old boot still answers", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.set(host.status, snapshot({ state: "error", error: "Signature mismatch" }));
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "host-error", message: "Signature mismatch", runningVersion: "0.6.2" },
      });
    }),
  );

  it.effect("reports the host's error from a new boot on the old version", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.set(
        host.status,
        snapshot({ state: "installing", installStage: "creating-verified-backup" }),
      );
      yield* TestClock.adjust("1 second");
      yield* Ref.set(host.status, snapshot({ state: "error", error: "Verified backup timed out" }));
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.2", progress: true },
        "10 seconds",
      );
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: {
          kind: "host-error",
          message: "Verified backup timed out",
          runningVersion: "0.6.2",
        },
      });
    }),
  );

  it.effect("fails as restarted on the old version when a new boot has no error", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* TestClock.adjust("1 second");
      yield* Ref.set(host.status, snapshot({ state: "idle" }));
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.2", progress: true },
        "10 seconds",
      );
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "wrong-version", runningVersion: "0.6.2", targetVersion: "0.6.4" },
      });
    }),
  );

  it.effect("gives up when the host has not come back within the restart budget", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* TestClock.adjust("1 second");
      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "backoff" as const }));
      yield* TestClock.adjust(REMOTE_UPDATE_RESTART_BUDGET_MS + 2_000);
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "not-back" },
      });
    }),
  );

  it.effect("asks the supervisor to retry at most every five seconds while waiting", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* TestClock.adjust("1 second");
      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "backoff" as const }));
      yield* TestClock.adjust("60 seconds");
      const retries = yield* Ref.get(host.retries);
      expect(retries).toBeGreaterThanOrEqual(11);
      expect(retries).toBeLessThanOrEqual(13);
      yield* Fiber.interrupt(host.run);
    }),
  );

  it.effect("keeps waiting when the same boot answers again after a network blip", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.set(host.status, snapshot({ state: "downloading", downloadPercent: 10 }));
      yield* host.restart(OLD_BOOT, "3 seconds");
      yield* TestClock.adjust("2 seconds");
      yield* Ref.set(host.status, snapshot({ state: "downloading", downloadPercent: 60 }));
      yield* TestClock.adjust("1 second");
      const states = yield* Ref.get(host.published);
      expect(states.at(-1)).toEqual({ phase: "downloading", percent: 60, targetVersion: "0.6.4" });
      yield* Fiber.interrupt(host.run);
    }),
  );

  it.effect("ends as already up to date", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        install: Effect.succeed(
          snapshot({ state: "up-to-date", latestVersion: null, targetVersion: null }),
        ),
      });
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "up-to-date" });
    }),
  );

  it.effect("refuses a manual host with the typed install error", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        install: Effect.fail(new RemoteUpdateInstallError({ code: REMOTE_UPDATE_MANUAL_REQUIRED })),
      });
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "manual-required" },
      });
    }),
  );

  it.effect("decides by version alone, with no percent or stage, without the capability", () =>
    Effect.gen(function* () {
      const legacy: RemoteUpdateServerIdentity = {
        bootId: null,
        serverVersion: "0.6.2",
        progress: false,
      };
      const host = yield* makeFakeHost({ identity: legacy });
      yield* Ref.set(host.status, snapshot({ state: "downloading", downloadPercent: 42 }));
      yield* TestClock.adjust("1 second");
      yield* Ref.set(
        host.status,
        snapshot({ state: "installing", installStage: "creating-verified-backup" }),
      );
      yield* TestClock.adjust("1 second");
      yield* host.restart({ bootId: null, serverVersion: "0.6.4", progress: false }, "10 seconds");
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "succeeded", version: "0.6.4" });
      const states = yield* Ref.get(host.published);
      expect(states).toContainEqual({
        phase: "downloading",
        percent: null,
        targetVersion: "0.6.4",
      });
      expect(states).toContainEqual({ phase: "installing", stage: null, targetVersion: "0.6.4" });
    }),
  );

  it.effect("does not retry a slow connecting attempt and resumes retries only in backoff", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "connecting" as const }));
      yield* TestClock.adjust("60 seconds");
      expect(yield* Ref.get(host.retries)).toBe(0);
      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "backoff" as const }));
      yield* TestClock.adjust("60 seconds");
      const retries = yield* Ref.get(host.retries);
      expect(retries).toBeGreaterThanOrEqual(11);
      expect(retries).toBeLessThanOrEqual(13);
      const times = yield* Ref.get(host.retryTimes);
      for (let index = 1; index < times.length; index += 1) {
        expect(times[index]! - times[index - 1]!).toBeGreaterThanOrEqual(
          REMOTE_UPDATE_RETRY_INTERVAL_MS,
        );
      }
      yield* Fiber.interrupt(host.run);
    }),
  );

  it.effect("ends within one poll when the restarting connection is blocked", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "backoff" as const }));
      yield* TestClock.adjust("1 second");
      const message = "This server's Coddy Studio version is not supported by this app.";
      yield* Ref.update(host.connection, (view) => ({
        ...view,
        phase: "blocked" as const,
        blockedMessage: message,
      }));
      yield* TestClock.adjust(REMOTE_UPDATE_POLL_MS);
      expect(host.run.pollUnsafe()).toBeDefined();
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "blocked", message },
      });
    }),
  );

  it.effect("reports an uncertain install timeout while the old boot still answers", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.set(host.status, snapshot({ state: "installing", installStage: "installing" }));
      yield* TestClock.adjust(REMOTE_UPDATE_INSTALL_BUDGET_MS + REMOTE_UPDATE_POLL_MS);
      expect(host.run.pollUnsafe()).toBeDefined();
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "install-timeout", targetVersion: "0.6.4" },
      });
    }),
  );

  it.effect("reports a download timeout with the old running version", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* TestClock.adjust(REMOTE_UPDATE_DOWNLOAD_BUDGET_MS + REMOTE_UPDATE_POLL_MS);
      expect(host.run.pollUnsafe()).toBeDefined();
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: {
          kind: "host-error",
          message: "the download did not finish in 10 minutes",
          runningVersion: "0.6.2",
        },
      });
    }),
  );

  it.effect("detects a new connected epoch even when the disconnected phase was not observed", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* Ref.set(host.identity, { bootId: "boot-2", serverVersion: "0.6.4", progress: true });
      yield* Ref.update(host.connection, (view) => ({
        ...view,
        connectedEpoch: view.connectedEpoch + 1,
      }));
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "succeeded", version: "0.6.4" });
      expect(yield* Ref.get(host.retries)).toBe(0);
    }),
  );

  it.effect("bounds a hung identity read by the exact restart deadline", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        readIdentity: (identity, call) => (call === 1 ? Effect.succeed(identity) : Effect.never),
      });
      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "backoff" as const }));
      yield* TestClock.adjust(REMOTE_UPDATE_POLL_MS);
      const restartStartedAt = yield* Clock.currentTimeMillis;
      // Reconnect with less than the ordinary ten-second read timeout remaining.
      yield* TestClock.adjust(REMOTE_UPDATE_RESTART_BUDGET_MS - 5_500);
      yield* Ref.update(host.connection, (view) => ({
        ...view,
        phase: "connected" as const,
        connectedEpoch: view.connectedEpoch + 1,
      }));
      yield* TestClock.adjust(5_499);
      expect(host.run.pollUnsafe()).toBeUndefined();
      yield* TestClock.adjust(1);
      expect(host.run.pollUnsafe()).toBeDefined();
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "not-back" },
      });
      expect((yield* Clock.currentTimeMillis) - restartStartedAt).toBe(
        REMOTE_UPDATE_RESTART_BUDGET_MS,
      );
    }),
  );

  it.effect("retries an inconclusive identity read on the same connected epoch", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        readIdentity: (identity, call) =>
          call === 2 || call === 3
            ? Effect.fail(new IdentityReadError())
            : Effect.succeed(identity),
      });
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.4", progress: true },
        "1 seconds",
      );
      yield* TestClock.adjust("3 seconds");
      expect(host.run.pollUnsafe()).toBeDefined();
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "succeeded", version: "0.6.4" });
      const phases = yield* host.phases;
      const verificationPhases = phases.slice(phases.indexOf("verifying"));
      expect(verificationPhases).not.toContain("restarting");
      expect(verificationPhases).toEqual(["verifying", "succeeded"]);
    }),
  );

  it.effect("publishes restarting once when a verifying connection drops before a new epoch", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        readIdentity: (identity, call) =>
          call === 2 ? Effect.fail(new IdentityReadError()) : Effect.succeed(identity),
      });
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.4", progress: true },
        "1 seconds",
      );
      yield* TestClock.adjust("1 second");
      const beforeDisconnect = yield* host.phases;
      expect(beforeDisconnect.at(-1)).toBe("verifying");

      yield* Ref.update(host.connection, (view) => ({ ...view, phase: "backoff" as const }));
      yield* TestClock.adjust("5 seconds");
      const whileDisconnected = yield* host.phases;
      expect(whileDisconnected.slice(beforeDisconnect.length)).toEqual(["restarting"]);

      yield* host.restart(
        { bootId: "boot-3", serverVersion: "0.6.4", progress: true },
        "1 seconds",
      );
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "succeeded", version: "0.6.4" });
      const phases = yield* host.phases;
      expect(phases.slice(phases.indexOf("verifying"))).toEqual([
        "verifying",
        "restarting",
        "verifying",
        "succeeded",
      ]);
    }),
  );

  it.effect("discards a successful identity reply from a superseded connection", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        readIdentity: (identity, call) =>
          call === 2
            ? Effect.sleep("2 seconds").pipe(Effect.as(identity))
            : Effect.succeed(identity),
      });
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.4", progress: true },
        "1 seconds",
      );
      yield* TestClock.adjust("1 second");
      yield* Ref.set(host.identity, { bootId: "boot-3", serverVersion: "0.6.2", progress: true });
      yield* Ref.set(host.status, snapshot({ state: "idle" }));
      yield* Ref.update(host.connection, (view) => ({
        ...view,
        connectedEpoch: view.connectedEpoch + 1,
      }));
      yield* TestClock.adjust("3 seconds");
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: { kind: "wrong-version", runningVersion: "0.6.2", targetVersion: "0.6.4" },
      });
      expect(yield* host.phases).not.toContain("succeeded");
    }),
  );

  it.effect("discards a failure probe from a superseded connection", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({
        readStatus: (status) =>
          status.state === "error"
            ? Effect.sleep("2 seconds").pipe(Effect.as(status))
            : Effect.succeed(status),
      });
      yield* Ref.set(host.status, snapshot({ state: "error", error: "Old attempt failed" }));
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.2", progress: true },
        "1 seconds",
      );
      yield* TestClock.adjust("1 second");
      yield* Ref.set(host.identity, { bootId: "boot-3", serverVersion: "0.6.4", progress: true });
      yield* Ref.set(host.status, snapshot({ state: "idle" }));
      yield* Ref.update(host.connection, (view) => ({
        ...view,
        connectedEpoch: view.connectedEpoch + 1,
      }));
      yield* TestClock.adjust("3 seconds");
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "succeeded", version: "0.6.4" });
      expect(yield* host.phases).not.toContain("failed");
    }),
  );

  for (const phase of ["available", "offline"] as const) {
    it.effect(`waits without reconnecting while ${phase}`, () =>
      Effect.gen(function* () {
        const host = yield* makeFakeHost();
        yield* Ref.update(host.connection, (view) => ({ ...view, phase }));
        yield* TestClock.adjust("60 seconds");
        expect(host.run.pollUnsafe()).toBeUndefined();
        expect(yield* Ref.get(host.retries)).toBe(0);
        yield* TestClock.adjust(REMOTE_UPDATE_RESTART_BUDGET_MS - 60_000 + REMOTE_UPDATE_POLL_MS);
        expect(yield* Fiber.join(host.run)).toEqual({
          phase: "failed",
          failure: { kind: "not-back" },
        });
        expect(yield* Ref.get(host.retries)).toBe(0);
      }),
    );
  }

  it.effect("times out the initial identity read before asking to install", () =>
    Effect.gen(function* () {
      const installs = yield* Ref.make(0);
      const host = yield* makeFakeHost({
        readIdentity: () => Effect.never,
        install: Ref.update(installs, (count) => count + 1).pipe(Effect.as(snapshot())),
      });
      yield* TestClock.adjust(REMOTE_UPDATE_READ_TIMEOUT_MS);
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: {
          kind: "host-error",
          message: "the server did not answer before the update started",
          runningVersion: "unknown",
        },
      });
      expect(yield* Ref.get(installs)).toBe(0);
    }),
  );

  it.effect("bounds a hung install request to thirty seconds", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost({ install: Effect.never });
      yield* TestClock.adjust(REMOTE_UPDATE_INSTALL_TIMEOUT_MS);
      const outcome = yield* Fiber.join(host.run);
      expect(outcome).toEqual({
        phase: "failed",
        failure: {
          kind: "host-error",
          message: "the install request was not accepted",
          runningVersion: "0.6.2",
        },
      });
      expect((yield* Ref.get(host.published)).at(-1)).toEqual(outcome);
    }),
  );

  it.effect("accepts a newer version using semantic version ordering", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* host.restart(
        { bootId: "boot-2", serverVersion: "0.6.10", progress: true },
        "2 seconds",
      );
      yield* TestClock.adjust("1 second");
      expect(yield* Fiber.join(host.run)).toEqual({ phase: "succeeded", version: "0.6.10" });
    }),
  );

  it.effect("requires a changed boot even when the target version already answers", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* host.restart({ ...OLD_BOOT, serverVersion: "0.6.4" }, "2 seconds");
      yield* TestClock.adjust("1 second");
      expect(host.run.pollUnsafe()).toBeUndefined();
      expect((yield* Ref.get(host.published)).at(-1)?.phase).toBe("downloading");
      yield* Fiber.interrupt(host.run);
    }),
  );

  it.effect("keeps the original download budget across a same-boot reconnect", () =>
    Effect.gen(function* () {
      const host = yield* makeFakeHost();
      yield* TestClock.adjust(REMOTE_UPDATE_DOWNLOAD_BUDGET_MS - 10_000);
      yield* host.restart(OLD_BOOT, "3 seconds");
      yield* TestClock.adjust("7 seconds");
      expect(host.run.pollUnsafe()).toBeDefined();
      expect(yield* Fiber.join(host.run)).toEqual({
        phase: "failed",
        failure: {
          kind: "host-error",
          message: "the download did not finish in 10 minutes",
          runningVersion: "0.6.2",
        },
      });
    }),
  );
});

describe("isRemoteUpdateRunActive", () => {
  it("keeps queued and in-progress runs active, and releases every terminal state", () => {
    const active: ReadonlyArray<RemoteUpdateRunState> = [
      { phase: "queued" },
      { phase: "starting" },
      { phase: "downloading", percent: null, targetVersion: null },
      { phase: "installing", stage: null, targetVersion: null },
      { phase: "restarting", targetVersion: null },
      { phase: "verifying", targetVersion: null },
    ];
    for (const state of active) expect(isRemoteUpdateRunActive(state)).toBe(true);
    for (const state of [
      null,
      { phase: "succeeded", version: "0.6.4" },
      { phase: "up-to-date" },
      { phase: "failed", failure: { kind: "not-back" } },
    ] as const) {
      expect(isRemoteUpdateRunActive(state)).toBe(false);
    }
  });
});
