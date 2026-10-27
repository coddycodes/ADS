import { ORCHESTRATION_WS_METHODS, WS_METHODS } from "@coddy/contracts";
import * as Cause from "effect/Cause";
import * as Clock from "effect/Clock";
import * as Context from "effect/Context";
import * as Duration from "effect/Duration";
import * as Effect from "effect/Effect";
import * as Option from "effect/Option";
import * as Schedule from "effect/Schedule";
import * as Schema from "effect/Schema";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { RpcClient, RpcClientError } from "effect/rpc";

import { EnvironmentSupervisor } from "../connection/supervisor.ts";
import type { WsRpcProtocolClient } from "../rpc/protocol.ts";
import { PRE_VERIFICATION_SUBSCRIPTIONS, type RpcSession } from "../rpc/session.ts";

export class EnvironmentRpcUnavailableError extends Schema.TaggedError<EnvironmentRpcUnavailableError>()(
  "EnvironmentRpcUnavailableError",
  {
    environmentId: Schema.String,
    message: Schema.String,
  },
) {}

export interface EnvironmentRpcRequestObservation {
  readonly environmentId: string;
  readonly method: string;
}

export interface EnvironmentRpcRequestObservationHandle {
  /** Runs once the request settles, however it ends. */
  readonly complete: Effect.Effect<void>;
  /** RPC headers to send with the request; empty for none. */
  readonly headers: Readonly<Record<string, string>>;
}

const UNOBSERVED: EnvironmentRpcRequestObservationHandle = { complete: Effect.void, headers: {} };

export class EnvironmentRpcRequestObserver extends Context.Reference<{
  readonly observe: (
    request: EnvironmentRpcRequestObservation,
  ) => Effect.Effect<EnvironmentRpcRequestObservationHandle>;
}>("@coddy/client-runtime/rpc/EnvironmentRpcRequestObserver", {
  defaultValue: () => ({
    observe: () => Effect.succeed(UNOBSERVED),
  }),
}) {}

export type EnvironmentRpcTag = keyof WsRpcProtocolClient & string;
type RpcMethod<TTag extends EnvironmentRpcTag> = WsRpcProtocolClient[TTag];

export type EnvironmentSubscriptionRpcTag =
  | typeof ORCHESTRATION_WS_METHODS.subscribeShell
  | typeof ORCHESTRATION_WS_METHODS.subscribeThread
  | typeof WS_METHODS.subscribeActivity
  | typeof WS_METHODS.subscribeAuthAccess
  | typeof WS_METHODS.subscribeServerConfig
  | typeof WS_METHODS.subscribeServerLifecycle
  | typeof WS_METHODS.subscribeTerminalEvents
  | typeof WS_METHODS.subscribeTerminalMetadata
  | typeof WS_METHODS.subscribePreviewEvents
  | typeof WS_METHODS.subscribeDiscoveredLocalServers
  | typeof WS_METHODS.subscribeGitManagerSignal
  | typeof WS_METHODS.subscribeWorktreeCatalog
  | typeof WS_METHODS.pullRequestsSubscribe
  | typeof WS_METHODS.previewAutomationConnect
  | typeof WS_METHODS.subscribeVcsStatus
  | typeof WS_METHODS.subscribeVcsStatusSummary
  | typeof WS_METHODS.projectsSubscribeEntries
  | typeof WS_METHODS.terminalAttach
  | typeof WS_METHODS.deviceSubscribeCatalog
  | typeof WS_METHODS.deviceSubscribeFrames;

export type EnvironmentStreamCommandRpcTag =
  | typeof WS_METHODS.cloudInstallRelayClient
  | typeof WS_METHODS.gitRunStackedAction
  | typeof WS_METHODS.gitManagerRunOperation
  | typeof WS_METHODS.previewTunnelOpen;

export type EnvironmentStreamRpcTag =
  | EnvironmentSubscriptionRpcTag
  | EnvironmentStreamCommandRpcTag;

export type EnvironmentUnaryRpcTag = Exclude<EnvironmentRpcTag, EnvironmentStreamRpcTag>;
const isRpcClientError = Schema.is(RpcClientError.RpcClientError);

export type EnvironmentRpcInput<TTag extends EnvironmentRpcTag> = Parameters<RpcMethod<TTag>>[0];

export type EnvironmentRpcSuccess<TTag extends EnvironmentUnaryRpcTag> =
  RpcMethod<TTag> extends (input: any, options?: any) => Effect.Effect<infer A, any, any>
    ? A
    : never;

export type EnvironmentRpcFailure<TTag extends EnvironmentUnaryRpcTag> =
  RpcMethod<TTag> extends (input: any, options?: any) => Effect.Effect<any, infer E, any>
    ? E
    : never;

export type EnvironmentRpcStreamValue<TTag extends EnvironmentStreamRpcTag> =
  RpcMethod<TTag> extends (input: any, options?: any) => Stream.Stream<infer A, any, any>
    ? A
    : never;

export type EnvironmentRpcStreamFailure<TTag extends EnvironmentStreamRpcTag> =
  RpcMethod<TTag> extends (input: any, options?: any) => Stream.Stream<any, infer E, any>
    ? E
    : never;

export interface EnvironmentSubscriptionOptions<TTag extends EnvironmentSubscriptionRpcTag> {
  readonly onExpectedFailure?: (
    cause: Cause.Cause<EnvironmentRpcStreamFailure<TTag>>,
  ) => Effect.Effect<void, never, never>;
  readonly retryExpectedFailureAfter?: Duration.Input;
  /** Selective exponential retry; nonmatching failures still fail the subscription. */
  readonly retryExpectedFailure?: {
    readonly when: (error: EnvironmentRpcStreamFailure<TTag>) => boolean;
    readonly initialDelay: Duration.Input;
    readonly maxDelay: Duration.Input;
    /** Reset the delay after a subscription stays up this long, excluding retry waits. */
    readonly resetAfter: Duration.Input;
  };
}

export const currentSession = Effect.fn("EnvironmentRpc.currentSession")(function* () {
  const supervisor = yield* EnvironmentSupervisor;
  return yield* SubscriptionRef.get(supervisor.session).pipe(
    Effect.flatMap(
      Option.match({
        onNone: () =>
          Effect.fail(
            new EnvironmentRpcUnavailableError({
              environmentId: supervisor.target.environmentId,
              message: `${supervisor.target.label} is not connected.`,
            }),
          ),
        onSome: Effect.succeed,
      }),
    ),
  );
});

export const requestInSession = Effect.fn("EnvironmentRpc.requestInSession")(function* <
  TTag extends EnvironmentUnaryRpcTag,
>(session: RpcSession, environmentId: string, tag: TTag, input: EnvironmentRpcInput<TTag>) {
  yield* Effect.annotateCurrentSpan({
    "environment.id": environmentId,
    "rpc.method": tag,
  });
  const observer = yield* EnvironmentRpcRequestObserver;
  const method = session.client[tag] as (
    input: EnvironmentRpcInput<TTag>,
  ) => Effect.Effect<EnvironmentRpcSuccess<TTag>, EnvironmentRpcFailure<TTag>>;
  const observation = yield* observer.observe({
    environmentId,
    method: tag,
  });
  const call = method(input);
  return yield* (
    Object.keys(observation.headers).length === 0
      ? call
      : RpcClient.withHeaders(call, observation.headers)
  ).pipe(Effect.ensuring(observation.complete));
});

export const request = Effect.fn("EnvironmentRpc.request")(function* <
  TTag extends EnvironmentUnaryRpcTag,
>(tag: TTag, input: EnvironmentRpcInput<TTag>) {
  const supervisor = yield* EnvironmentSupervisor;
  const session = yield* currentSession();
  return yield* requestInSession(session, supervisor.target.environmentId, tag, input);
});

export function runStream<TTag extends EnvironmentStreamCommandRpcTag>(
  tag: TTag,
  input: EnvironmentRpcInput<TTag>,
): Stream.Stream<
  EnvironmentRpcStreamValue<TTag>,
  EnvironmentRpcStreamFailure<TTag> | EnvironmentRpcUnavailableError,
  EnvironmentSupervisor
> {
  return Stream.unwrap(
    currentSession().pipe(
      Effect.map((session) => {
        const method = session.client[tag] as (
          input: EnvironmentRpcInput<TTag>,
        ) => Stream.Stream<EnvironmentRpcStreamValue<TTag>, EnvironmentRpcStreamFailure<TTag>>;
        return method(input);
      }),
    ),
  ).pipe(
    Stream.withSpan("EnvironmentRpc.runStream", {
      attributes: { "rpc.method": tag },
    }),
  );
}

/**
 * A subscription's input, or a function read at every (re)subscribe, so a stream that resumes
 * from what it already holds can say so each time.
 */
export type EnvironmentSubscriptionInput<TTag extends EnvironmentSubscriptionRpcTag> =
  | EnvironmentRpcInput<TTag>
  | (() => EnvironmentRpcInput<TTag>);

export function subscribeInSession<TTag extends EnvironmentSubscriptionRpcTag>(
  session: RpcSession,
  environmentId: string,
  tag: TTag,
  input: EnvironmentSubscriptionInput<TTag>,
  options?: EnvironmentSubscriptionOptions<TTag>,
): Stream.Stream<EnvironmentRpcStreamValue<TTag>, EnvironmentRpcStreamFailure<TTag>> {
  const method = session.client[tag] as (
    input: EnvironmentRpcInput<TTag>,
  ) => Stream.Stream<EnvironmentRpcStreamValue<TTag>, EnvironmentRpcStreamFailure<TTag>>;
  return Stream.suspend(() => {
    const retry = options?.retryExpectedFailure;
    const maxDelay = retry === undefined ? 0 : Duration.toMillis(retry.maxDelay);
    const initialDelay =
      retry === undefined ? 0 : Math.min(Duration.toMillis(retry.initialDelay), maxDelay);
    let retryDelay = initialDelay;
    let retryRequested = false;
    const subscribeToSession = (): Stream.Stream<
      EnvironmentRpcStreamValue<TTag>,
      EnvironmentRpcStreamFailure<TTag>
    > =>
      Stream.unwrap(
        Effect.gen(function* () {
          retryRequested = false;
          const subscribedAt = yield* Clock.currentTimeMillis;
          return method(typeof input === "function" ? input() : input).pipe(
            Stream.catchCause((cause) => {
              const hasOnlyExpectedFailures =
                cause.reasons.length > 0 && cause.reasons.every((reason) => reason._tag === "Fail");
              const isTransportFailure =
                hasOnlyExpectedFailures &&
                cause.reasons.every(
                  (reason) => reason._tag === "Fail" && isRpcClientError(reason.error),
                );
              if (isTransportFailure) {
                return Stream.fromEffect(
                  Effect.logWarning(
                    "Durable RPC subscription lost its transport; waiting for the next session.",
                    {
                      cause: Cause.pretty(cause),
                      method: tag,
                      environmentId,
                    },
                  ),
                ).pipe(Stream.drain);
              }
              if (hasOnlyExpectedFailures && retry !== undefined) {
                if (
                  !cause.reasons.every(
                    (reason) => reason._tag === "Fail" && retry.when(reason.error),
                  )
                ) {
                  return Stream.failCause(cause);
                }
                return Stream.fromEffect(
                  Effect.gen(function* () {
                    const now = yield* Clock.currentTimeMillis;
                    const delay =
                      now - subscribedAt >= Duration.toMillis(retry.resetAfter)
                        ? initialDelay
                        : retryDelay;
                    if (options?.onExpectedFailure !== undefined) {
                      yield* options.onExpectedFailure(cause);
                    }
                    yield* Effect.sleep(delay);
                    retryDelay = Math.min(delay * 2, maxDelay);
                    retryRequested = true;
                  }),
                ).pipe(Stream.drain);
              }
              if (hasOnlyExpectedFailures && options?.onExpectedFailure !== undefined) {
                const handled = Stream.fromEffect(options.onExpectedFailure(cause)).pipe(
                  Stream.drain,
                );
                if (options.retryExpectedFailureAfter === undefined) {
                  return handled;
                }
                return handled.pipe(
                  Stream.concat(
                    Stream.fromEffect(
                      Effect.sleep(options.retryExpectedFailureAfter).pipe(
                        Effect.andThen(
                          Effect.sync(() => {
                            retryRequested = true;
                          }),
                        ),
                      ),
                    ).pipe(Stream.drain),
                  ),
                );
              }
              return Stream.failCause(cause);
            }),
          );
        }),
      );
    const stream = subscribeToSession();
    // Close each completed attempt before repeating; recursive recovery retains its scopes.
    return retry === undefined && options?.retryExpectedFailureAfter === undefined
      ? stream
      : stream.pipe(Stream.repeat(Schedule.forever.pipe(Schedule.while(() => retryRequested))));
  });
}

export function subscribe<TTag extends EnvironmentSubscriptionRpcTag>(
  tag: TTag,
  input: EnvironmentSubscriptionInput<TTag>,
  options?: EnvironmentSubscriptionOptions<TTag>,
): Stream.Stream<
  EnvironmentRpcStreamValue<TTag>,
  EnvironmentRpcStreamFailure<TTag>,
  EnvironmentSupervisor
> {
  return Stream.unwrap(
    EnvironmentSupervisor.pipe(
      Effect.map((supervisor) =>
        // A read subscription follows the session from `configuring` on (see
        // `EnvironmentSupervisor.subscriptionSession`), a round trip sooner.
        SubscriptionRef.changes(
          PRE_VERIFICATION_SUBSCRIPTIONS.has(tag)
            ? (supervisor.subscriptionSession ?? supervisor.session)
            : supervisor.session,
        ).pipe(
          Stream.switchMap(
            Option.match({
              onNone: () => Stream.empty,
              onSome: (session) =>
                subscribeInSession(session, supervisor.target.environmentId, tag, input, options),
            }),
          ),
        ),
      ),
    ),
  ).pipe(
    Stream.withSpan("EnvironmentRpc.subscribe", {
      attributes: { "rpc.method": tag },
    }),
  );
}

export const config = Effect.gen(function* () {
  const session = yield* currentSession();
  return yield* session.initialConfig;
}).pipe(Effect.withSpan("EnvironmentRpc.config"));
