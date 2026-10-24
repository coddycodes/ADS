import type { ServerConfig, ServerConfigStreamEvent } from "@bibcode/contracts";
import * as Deferred from "effect/Deferred";
import * as Effect from "effect/Effect";
import type * as Scope from "effect/Scope";
import * as Stream from "effect/Stream";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { RpcClientDefect, RpcClientError } from "effect/rpc/RpcClientError";

/** The config after one stream event. Updates before the first snapshot change nothing. */
export function applyServerConfigEvent(
  config: ServerConfig | null,
  event: ServerConfigStreamEvent,
): ServerConfig | null {
  switch (event.type) {
    case "snapshot":
      return event.config;
    case "keybindingsUpdated":
      return config === null
        ? null
        : { ...config, keybindings: event.payload.keybindings, issues: event.payload.issues };
    case "providerStatuses":
      return config === null ? null : { ...config, providers: event.payload.providers };
    case "settingsUpdated":
      return config === null ? null : { ...config, settings: event.payload.settings };
  }
}

interface ServerConfigFeed {
  readonly config: ServerConfig | null;
  readonly event: ServerConfigStreamEvent | null;
}

export interface SharedServerConfig<E> {
  /** The first snapshot's config, or the stream's failure. */
  readonly initialConfig: Effect.Effect<ServerConfig, E | RpcClientError>;
  /** One snapshot of the current config, then every later event unchanged. */
  readonly events: Stream.Stream<ServerConfigStreamEvent, E | RpcClientError>;
}

/**
 * Runs a connection's one `subscribeServerConfig` stream and serves it to
 * every consumer, so the config (about 250 KB) crosses the wire once per
 * connection. A late subscriber starts from a synthesized snapshot of the
 * current config, then sees the same events as everyone else. A source that
 * ends or fails settles readiness and every subscriber with that failure.
 */
export const makeSharedServerConfig = <E>(
  source: Stream.Stream<ServerConfigStreamEvent, E>,
): Effect.Effect<SharedServerConfig<E>, never, Scope.Scope> =>
  Effect.gen(function* () {
    const feed = yield* SubscriptionRef.make<ServerConfigFeed>({ config: null, event: null });
    const firstConfig = yield* Deferred.make<ServerConfig, E | RpcClientError>();
    const failed = yield* Deferred.make<never, E | RpcClientError>();
    yield* source.pipe(
      Stream.runForEach((event) =>
        SubscriptionRef.updateAndGet(feed, (state) => ({
          config: applyServerConfigEvent(state.config, event),
          event,
        })).pipe(
          Effect.flatMap((state) =>
            state.config === null ? Effect.void : Deferred.succeed(firstConfig, state.config),
          ),
        ),
      ),
      Effect.andThen(
        Effect.fail(
          new RpcClientError({
            reason: new RpcClientDefect({
              message: "The server config stream ended.",
              cause: undefined,
            }),
          }),
        ),
      ),
      Effect.catch((error: E | RpcClientError) =>
        Deferred.fail(firstConfig, error).pipe(Effect.andThen(Deferred.fail(failed, error))),
      ),
      Effect.forkScoped,
    );
    const events: Stream.Stream<ServerConfigStreamEvent, E | RpcClientError> = Stream.merge(
      SubscriptionRef.changes(feed).pipe(
        Stream.mapAccum(
          () => false,
          (started, state): readonly [boolean, ReadonlyArray<ServerConfigStreamEvent>] => {
            if (!started) {
              return state.config === null
                ? [false, []]
                : [true, [{ version: 1, type: "snapshot", config: state.config }]];
            }
            return [true, state.event === null ? [] : [state.event]];
          },
        ),
      ),
      Stream.fromEffect(Deferred.await(failed)),
    );
    return { initialConfig: Deferred.await(firstConfig), events };
  });
