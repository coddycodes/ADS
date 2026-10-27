import {
  REMOTE_UPDATE_MANUAL_REQUIRED,
  RemoteUpdateInstallError,
  type RemoteUpdateSnapshot,
} from "@coddy/contracts";
import { compareSemverVersions } from "@coddy/shared/semver";
import * as Clock from "effect/Clock";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Option from "effect/Option";
import * as Schema from "effect/Schema";

import type { SupervisorConnectionPhase } from "../connection/model.ts";

export interface RemoteUpdateConnectionView {
  /** The supervisor's phase for this environment. */
  readonly phase: SupervisorConnectionPhase;
  /** Increases each time the environment enters `connected`; never decreases. */
  readonly connectedEpoch: number;
  /** The blocking failure's message while `phase === "blocked"`, else null. */
  readonly blockedMessage: string | null;
}

export interface RemoteUpdateServerIdentity {
  readonly bootId: string | null;
  readonly serverVersion: string;
  /** `capabilities.remoteUpdateProgress`: bootId, percent, stages and counts are reported. */
  readonly progress: boolean;
}

export interface RemoteUpdatePort {
  /** The latest supervisor state for the environment (fed from `stateChanges`). */
  readonly connection: Effect.Effect<RemoteUpdateConnectionView>;
  /** `server.getConfig` on the live connection. */
  readonly identity: Effect.Effect<RemoteUpdateServerIdentity, unknown>;
  readonly status: Effect.Effect<RemoteUpdateSnapshot, unknown>;
  readonly install: Effect.Effect<RemoteUpdateSnapshot, unknown>;
  readonly retryNow: Effect.Effect<void>;
}

export type RemoteUpdateFailure =
  | { readonly kind: "host-error"; readonly message: string; readonly runningVersion: string }
  | { readonly kind: "not-back" }
  | {
      readonly kind: "wrong-version";
      readonly runningVersion: string;
      readonly targetVersion: string;
    }
  | { readonly kind: "manual-required" }
  | { readonly kind: "install-timeout"; readonly targetVersion: string | null }
  | { readonly kind: "blocked"; readonly message: string };

export type RemoteUpdateRunState =
  | { readonly phase: "queued" }
  | { readonly phase: "starting" }
  | {
      readonly phase: "downloading";
      readonly percent: number | null;
      readonly targetVersion: string | null;
    }
  | {
      readonly phase: "installing";
      readonly stage: string | null;
      readonly targetVersion: string | null;
    }
  | { readonly phase: "restarting"; readonly targetVersion: string | null }
  | { readonly phase: "verifying"; readonly targetVersion: string | null }
  | { readonly phase: "succeeded"; readonly version: string }
  | { readonly phase: "up-to-date" }
  | { readonly phase: "failed"; readonly failure: RemoteUpdateFailure };

export const REMOTE_UPDATE_POLL_MS = 1_000;
export const REMOTE_UPDATE_READ_TIMEOUT_MS = 10_000;
export const REMOTE_UPDATE_INSTALL_TIMEOUT_MS = 30_000;
export const REMOTE_UPDATE_DOWNLOAD_BUDGET_MS = 10 * 60_000;
/** 30 s drain, 45 s prepare, 10 s commit, plus stopping and the installer. */
export const REMOTE_UPDATE_INSTALL_BUDGET_MS = 2 * 60_000;
export const REMOTE_UPDATE_RESTART_BUDGET_MS = 3 * 60_000;
export const REMOTE_UPDATE_RETRY_INTERVAL_MS = 5_000;

const isInstallError = Schema.is(RemoteUpdateInstallError);

export function isRemoteUpdateRunActive(state: RemoteUpdateRunState | null): boolean {
  switch (state?.phase) {
    case "queued":
    case "starting":
    case "downloading":
    case "installing":
    case "restarting":
    case "verifying":
      return true;
    default:
      return false;
  }
}

type Verdict =
  | { readonly kind: "done"; readonly state: RemoteUpdateRunState }
  | { readonly kind: "same-boot"; readonly snapshot: RemoteUpdateSnapshot | null }
  | { readonly kind: "pending" };

/** Each read is capped at ten seconds and whatever is left of the current budget. */
const read = Effect.fn("RemoteUpdate.read")(function* <A, E>(
  effect: Effect.Effect<A, E>,
  deadline: number,
): Effect.fn.Return<A | null> {
  const remaining = deadline - (yield* Clock.currentTimeMillis);
  if (remaining <= 0) return null;
  const exit = yield* Effect.exit(
    effect.pipe(Effect.timeout(Math.min(REMOTE_UPDATE_READ_TIMEOUT_MS, remaining))),
  );
  return Exit.isSuccess(exit) ? exit.value : null;
});

const poll = Effect.fn("RemoteUpdate.poll")(function* (deadline: number) {
  const remaining = deadline - (yield* Clock.currentTimeMillis);
  if (remaining > 0) yield* Effect.sleep(Math.min(REMOTE_UPDATE_POLL_MS, remaining));
});

export const runRemoteUpdate = Effect.fn("RemoteUpdate.run")(function* (
  port: RemoteUpdatePort,
  publish: (state: RemoteUpdateRunState) => Effect.Effect<void>,
): Effect.fn.Return<RemoteUpdateRunState> {
  const finish = (state: RemoteUpdateRunState) => publish(state).pipe(Effect.as(state));
  const hostError = (message: string, runningVersion: string) =>
    finish({ phase: "failed", failure: { kind: "host-error", message, runningVersion } });

  yield* publish({ phase: "starting" });
  const startedAt = yield* Clock.currentTimeMillis;
  // @effect-diagnostics-next-line anyUnknownInErrorContext:off - The port accepts arbitrary failures; read settles them into a nullable result.
  const before = yield* read(port.identity, startedAt + REMOTE_UPDATE_READ_TIMEOUT_MS);
  if (before === null) {
    return yield* hostError("the server did not answer before the update started", "unknown");
  }
  const progress = before.progress && before.bootId !== null;

  const installExit = yield* Effect.exit(
    // @effect-diagnostics-next-line anyUnknownInErrorContext:off - The port's arbitrary install failure is classified from its Exit below.
    port.install.pipe(Effect.timeout(REMOTE_UPDATE_INSTALL_TIMEOUT_MS)),
  );
  if (Exit.isFailure(installExit)) {
    const error = Option.getOrNull(Exit.findErrorOption(installExit));
    return isInstallError(error) && error.code === REMOTE_UPDATE_MANUAL_REQUIRED
      ? yield* finish({ phase: "failed", failure: { kind: "manual-required" } })
      : yield* hostError("the install request was not accepted", before.serverVersion);
  }
  const epochAtInstall = (yield* port.connection).connectedEpoch;
  let connectedEpoch = epochAtInstall;
  let target = installExit.value.targetVersion ?? installExit.value.latestVersion;
  if (installExit.value.state === "up-to-date") return yield* finish({ phase: "up-to-date" });
  if (installExit.value.state === "error") {
    return yield* hostError(
      installExit.value.error ?? "the host's updater reported an error",
      before.serverVersion,
    );
  }

  const verify = Effect.fn("RemoteUpdate.verify")(function* (
    deadline: number,
    expectedEpoch: number,
  ): Effect.fn.Return<Verdict> {
    // @effect-diagnostics-next-line anyUnknownInErrorContext:off - The port accepts arbitrary failures; read settles them into a nullable result.
    const after = yield* read(port.identity, deadline);
    if (after === null) return { kind: "pending" };
    const identityConnection = yield* port.connection;
    if (
      identityConnection.phase !== "connected" ||
      identityConnection.connectedEpoch !== expectedEpoch
    ) {
      return { kind: "pending" };
    }
    const newBoot = progress && after.bootId !== null && after.bootId !== before.bootId;
    const reachedTarget =
      target !== null && compareSemverVersions(after.serverVersion, target) >= 0;
    if (reachedTarget && (newBoot || !progress)) {
      return { kind: "done", state: { phase: "succeeded", version: after.serverVersion } };
    }
    // Protection can restart the prior backends on a new boot after failing.
    // @effect-diagnostics-next-line anyUnknownInErrorContext:off - The port accepts arbitrary failures; read settles them into a nullable result.
    const status = yield* read(port.status, deadline);
    const statusConnection = yield* port.connection;
    if (
      statusConnection.phase !== "connected" ||
      statusConnection.connectedEpoch !== expectedEpoch
    ) {
      return { kind: "pending" };
    }
    if (status?.state === "error") {
      return {
        kind: "done",
        state: {
          phase: "failed",
          failure: {
            kind: "host-error",
            message: status.error ?? "the host's updater reported an error",
            runningVersion: after.serverVersion,
          },
        },
      };
    }
    if (newBoot) {
      return {
        kind: "done",
        state: {
          phase: "failed",
          failure: {
            kind: "wrong-version",
            runningVersion: after.serverVersion,
            targetVersion: target ?? after.serverVersion,
          },
        },
      };
    }
    return { kind: "same-boot", snapshot: status };
  });

  let stageDeadline = startedAt + REMOTE_UPDATE_DOWNLOAD_BUDGET_MS;
  let installing = false;
  const follow = Effect.fn("RemoteUpdate.follow")(function* (
    initial: RemoteUpdateSnapshot | null,
  ): Effect.fn.Return<Verdict> {
    let pending = initial;
    while (true) {
      const connection = yield* port.connection;
      if (connection.phase !== "connected" || connection.connectedEpoch !== connectedEpoch) {
        return { kind: "pending" };
      }
      // @effect-diagnostics-next-line anyUnknownInErrorContext:off - The port accepts arbitrary failures; read settles them into a nullable result.
      const snapshot = pending ?? (yield* read(port.status, stageDeadline));
      pending = null;
      const current = yield* port.connection;
      if (current.phase !== "connected" || current.connectedEpoch !== connectedEpoch) {
        return { kind: "pending" };
      }
      if (snapshot !== null) {
        target = snapshot.targetVersion ?? target;
        switch (snapshot.state) {
          case "downloading":
            yield* publish({
              phase: "downloading",
              percent: progress ? snapshot.downloadPercent : null,
              targetVersion: target,
            });
            break;
          case "installing":
            if (!installing) {
              installing = true;
              stageDeadline = (yield* Clock.currentTimeMillis) + REMOTE_UPDATE_INSTALL_BUDGET_MS;
            }
            yield* publish({
              phase: "installing",
              stage: progress ? snapshot.installStage : null,
              targetVersion: target,
            });
            break;
          case "up-to-date":
            return { kind: "done", state: { phase: "up-to-date" } };
          case "error":
            return {
              kind: "done",
              state: {
                phase: "failed",
                failure: {
                  kind: "host-error",
                  message: snapshot.error ?? "the host's updater reported an error",
                  runningVersion: snapshot.serverVersion,
                },
              },
            };
          default:
            break; // idle, checking or update-available: the flow has not started yet
        }
      }
      if ((yield* Clock.currentTimeMillis) >= stageDeadline) {
        return {
          kind: "done",
          state: {
            phase: "failed",
            failure: installing
              ? { kind: "install-timeout", targetVersion: target }
              : {
                  kind: "host-error",
                  message: "the download did not finish in 10 minutes",
                  runningVersion: before.serverVersion,
                },
          },
        };
      }
      yield* poll(stageDeadline);
    }
  });

  let resume: RemoteUpdateSnapshot | null = installExit.value;
  let lastRetry = Number.NEGATIVE_INFINITY;
  while (true) {
    const followed = yield* follow(resume);
    if (followed.kind === "done") return yield* finish(followed.state);

    yield* publish({ phase: "restarting", targetVersion: target });
    const restartDeadline = (yield* Clock.currentTimeMillis) + REMOTE_UPDATE_RESTART_BUDGET_MS;
    let publishedVerifyingEpoch: number | null = null;
    let verdict: Verdict = { kind: "pending" };
    while (verdict.kind === "pending") {
      const now = yield* Clock.currentTimeMillis;
      if (now >= restartDeadline) {
        return yield* finish({ phase: "failed", failure: { kind: "not-back" } });
      }
      const connection = yield* port.connection;
      if (connection.phase === "blocked") {
        return yield* finish({
          phase: "failed",
          failure: {
            kind: "blocked",
            message: connection.blockedMessage ?? "the connection is blocked",
          },
        });
      }
      if (connection.phase !== "connected" && publishedVerifyingEpoch !== null) {
        publishedVerifyingEpoch = null;
        yield* publish({ phase: "restarting", targetVersion: target });
      }
      if (connection.phase === "connected" && connection.connectedEpoch !== connectedEpoch) {
        if (publishedVerifyingEpoch !== connection.connectedEpoch) {
          publishedVerifyingEpoch = connection.connectedEpoch;
          yield* publish({ phase: "verifying", targetVersion: target });
        }
        verdict = yield* verify(restartDeadline, connection.connectedEpoch);
        if (verdict.kind !== "pending") {
          // An inconclusive read must not consume this epoch's verification opportunity.
          connectedEpoch = connection.connectedEpoch;
        }
      } else if (
        connection.phase === "backoff" &&
        now - lastRetry >= REMOTE_UPDATE_RETRY_INTERVAL_MS
      ) {
        // RetryRequested also interrupts connecting attempts and live leases.
        lastRetry = now;
        yield* port.retryNow;
      }
      if (verdict.kind === "pending") yield* poll(restartDeadline);
    }
    if (verdict.kind === "done") return yield* finish(verdict.state);
    // A network blip keeps the original stage budget and adopts the new epoch.
    resume = verdict.snapshot;
  }
});
