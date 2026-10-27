import {
  type GitCancelCloneInput,
  type GitCancelCloneResult,
  type GitCloneResult,
  WS_METHODS,
} from "@coddy/contracts";
import * as Effect from "effect/Effect";
import * as Exit from "effect/Exit";
import * as Schema from "effect/Schema";

import {
  isSessionTransportLoss,
  nextSession,
  type NextSession,
} from "../connection/nextSession.ts";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import { currentSession, requestInSession } from "../rpc/client.ts";
import type { RpcSession } from "../rpc/session.ts";

/** Where a clone stands, reported to the dialog. */
export interface VcsCloneProgress {
  readonly phase: "cloning" | "reconnecting";
  /**
   * The server keeps the clone across reconnects (`vcsCloneReattach`). Cancel and close must
   * then send `vcs.cancelClone`; an abort alone only stops following the clone.
   */
  readonly reattach: boolean;
}

export interface VcsCloneCommandInput {
  readonly url: string;
  readonly parentDir: string;
  readonly directoryName?: string | undefined;
  readonly onProgress?: ((progress: VcsCloneProgress) => void) | undefined;
}

/** The client stopped following a clone; the clone may still run on the server. */
export class VcsCloneStoppedError extends Schema.TaggedError<VcsCloneStoppedError>()(
  "VcsCloneStoppedError",
  {
    environmentId: Schema.String,
    reason: Schema.Literals(["environment-unavailable", "reattach-unsupported"]),
    message: Schema.String,
  },
) {}

export interface CloneDestinationInput {
  readonly url: string;
  readonly parentDir: string;
  readonly directoryName?: string | undefined;
}

/** The server's leaf rule (`clone_destination_leaf`): `directoryName`, else the URL's last
 * path segment without a trailing `.git`. */
export function cloneDestinationLeaf(url: string, directoryName: string | undefined): string {
  if (directoryName !== undefined) {
    return directoryName;
  }
  const segments = url.replace(/[\\/]+$/, "").split(/[\\/:]/);
  return (segments.at(-1) ?? "repository").replace(/(?:\.git)+$/, "");
}

/**
 * The key that holds a clone behind a pending `vcs.cancelClone`: environment, exact URL, and
 * leaf. The parent folder is left out on purpose. Only the server can canonicalize a host path
 * (`/code`, `/code/.`, `~/code`, `C:\\code` and `C:/code` may all name one folder), and the
 * server cancels a live clone only when its URL matches exactly. So a cancel can stop only a clone
 * of the same URL into the same leaf, and holding every such clone, whatever the parent's
 * spelling, costs at most one cancel round trip.
 */
export function pendingCloneCancelKey(environmentId: string, input: CloneDestinationInput): string {
  return JSON.stringify([
    environmentId,
    input.url,
    cloneDestinationLeaf(input.url, input.directoryName),
  ]);
}

/**
 * `vcs.cancelClone` requests still in flight in this client runtime, by
 * {@link pendingCloneCancelKey}. The server keys a cancel by destination (spec ruling 5), so a
 * cancel that is still retrying could stop a later clone of the same URL into the same folder: such
 * a clone waits here until every earlier cancel has settled. One registry per client runtime (a
 * tab or a desktop window).
 */
const pendingCloneCancels = new Map<string, Set<Promise<void>>>();

/** Records a cancel in flight for this clone. The returned function ends the record. */
export function registerCloneCancel(
  environmentId: string,
  input: CloneDestinationInput,
): () => void {
  const key = pendingCloneCancelKey(environmentId, input);
  let settle!: () => void;
  const settled = new Promise<void>((resolve) => {
    settle = resolve;
  });
  const pending = pendingCloneCancels.get(key) ?? new Set<Promise<void>>();
  pending.add(settled);
  pendingCloneCancels.set(key, pending);
  let released = false;
  return () => {
    if (released) return;
    released = true;
    pending.delete(settled);
    if (pending.size === 0 && pendingCloneCancels.get(key) === pending) {
      pendingCloneCancels.delete(key);
    }
    settle();
  };
}

const awaitPendingCloneCancels = Effect.fn("Vcs.awaitPendingCloneCancels")(function* (
  environmentId: string,
  input: CloneDestinationInput,
) {
  const key = pendingCloneCancelKey(environmentId, input);
  while (true) {
    const pending = pendingCloneCancels.get(key);
    if (pending === undefined || pending.size === 0) {
      return;
    }
    const snapshot = [...pending];
    yield* Effect.promise(() => Promise.all(snapshot));
  }
});

function cloneFields(input: VcsCloneCommandInput) {
  return {
    url: input.url,
    parentDir: input.parentDir,
    ...(input.directoryName === undefined ? {} : { directoryName: input.directoryName }),
  };
}

/**
 * Clone from URL. Without the capability: today's single call. With it: the server keeps the
 * clone when this socket drops (`detach`), and each later session re-attaches (`attach`) until
 * an outcome, a stopped environment, or a session without the capability.
 */
export const cloneWithReattach = Effect.fn("Vcs.cloneWithReattach")(function* (
  input: VcsCloneCommandInput,
) {
  const registry = yield* EnvironmentRegistry;
  let supervisor = yield* EnvironmentSupervisor;
  const environmentId = supervisor.target.environmentId;
  // A cancel of this URL into this leaf may still be retrying: dispatch nothing until every such
  // earlier cancel has settled, so none of them can stop this clone.
  yield* awaitPendingCloneCancels(environmentId, input);
  // The registry may have replaced the supervisor during that wait, so continue on its current
  // one. A clone started while disconnected still fails at once.
  supervisor = yield* registry.run(environmentId, EnvironmentSupervisor).pipe(
    Effect.catchTag("EnvironmentNotRegisteredError", () =>
      Effect.fail(
        new VcsCloneStoppedError({
          environmentId,
          reason: "environment-unavailable",
          message: `Can't reconnect to ${supervisor.target.label}.`,
        }),
      ),
    ),
  );
  const clone = cloneFields(input);
  let session = yield* currentSession().pipe(
    Effect.provideService(EnvironmentSupervisor, supervisor),
  );
  let attach = false;
  while (true) {
    const config = yield* Effect.exit(session.initialConfig);
    if (Exit.isSuccess(config)) {
      if (config.value.environment.capabilities.vcsCloneReattach !== true) {
        if (attach) {
          return yield* new VcsCloneStoppedError({
            environmentId,
            reason: "reattach-unsupported",
            message: `${supervisor.target.label} can no longer resume this clone.`,
          });
        }
        input.onProgress?.({ phase: "cloning", reattach: false });
        return yield* requestInSession(session, environmentId, WS_METHODS.vcsClone, clone);
      }
      input.onProgress?.({ phase: "cloning", reattach: true });
      const result = yield* Effect.exit(
        requestInSession(session, environmentId, WS_METHODS.vcsClone, {
          ...clone,
          detach: true,
          ...(attach ? { attach: true } : {}),
        }),
      );
      if (Exit.isSuccess(result)) {
        return result.value satisfies GitCloneResult;
      }
      if (!isSessionTransportLoss(result.cause)) {
        return yield* Effect.failCause(result.cause);
      }
    } else if (!attach) {
      return yield* Effect.failCause(config.cause);
    }
    input.onProgress?.({ phase: "reconnecting", reattach: true });
    const next: NextSession = yield* nextSession(registry, supervisor, session);
    if (next._tag === "Stopped") {
      return yield* new VcsCloneStoppedError({
        environmentId,
        reason: "environment-unavailable",
        message: `Can't reconnect to ${next.label}.`,
      });
    }
    supervisor = next.supervisor;
    session = next.session;
    attach = true;
  }
});

/**
 * `vcs.cancelClone`, sent on the current session or, while disconnected, on the next one that
 * advertises `vcsCloneReattach`. It runs on its own command lane because the clone holds the
 * destination's serial lane.
 */
export const cancelCloneOnNextSession = Effect.fn("Vcs.cancelCloneOnNextSession")(function* (
  input: GitCancelCloneInput,
) {
  const registry = yield* EnvironmentRegistry;
  let supervisor = yield* EnvironmentSupervisor;
  const environmentId = supervisor.target.environmentId;
  let lost: RpcSession | null = null;
  while (true) {
    const next: NextSession = yield* nextSession(registry, supervisor, lost);
    if (next._tag === "Stopped") {
      return yield* new VcsCloneStoppedError({
        environmentId,
        reason: "environment-unavailable",
        message: `Can't reconnect to ${next.label}.`,
      });
    }
    supervisor = next.supervisor;
    const config = yield* Effect.exit(next.session.initialConfig);
    if (Exit.isFailure(config)) {
      // A session that failed to configure is as good as lost: wait for the next one.
      lost = next.session;
      continue;
    }
    if (config.value.environment.capabilities.vcsCloneReattach !== true) {
      // A host without re-attach has restarted since the clone began, so the clone is gone;
      // sending the RPC would fail as an unknown method and leave the dialog cancelling.
      return yield* new VcsCloneStoppedError({
        environmentId,
        reason: "reattach-unsupported",
        message: `${supervisor.target.label} can no longer resume or cancel this clone.`,
      });
    }
    const result = yield* Effect.exit(
      requestInSession(next.session, environmentId, WS_METHODS.vcsCancelClone, input),
    );
    if (Exit.isSuccess(result)) {
      return result.value satisfies GitCancelCloneResult;
    }
    if (!isSessionTransportLoss(result.cause)) {
      return yield* Effect.failCause(result.cause);
    }
    lost = next.session;
  }
});
