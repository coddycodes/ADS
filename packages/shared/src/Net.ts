import * as NodeNet from "node:net";

import * as Data from "effect/Data";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Context from "effect/Context";
import * as Predicate from "effect/Predicate";

export class NetError extends Data.TaggedError("NetError")<{
  readonly message: string;
  readonly cause?: unknown;
}> {}

const isErrnoExceptionWithCode = (
  cause: unknown,
): cause is {
  readonly code: string;
} =>
  Predicate.isObject(cause) &&
  Predicate.hasProperty(cause, "code") &&
  Predicate.isString(cause.code);

const makeCloseServerOnce = (server: NodeNet.Server) => {
  let closeRequested = false;
  return (callback?: () => void) => {
    if (closeRequested) return;
    closeRequested = true;
    server.close(callback);
  };
};

const closeServer = (close: () => void) => {
  try {
    close();
  } catch {
    // Ignore close failures during cleanup.
  }
};

export interface NetServiceShape {
  /**
   * Returns true when a TCP server can bind to {host, port}.
   */
  readonly canListenOnHost: (port: number, host: string) => Effect.Effect<boolean>;

  /**
   * Checks loopback availability on both IPv4 and IPv6 localhost addresses.
   */
  readonly isPortAvailableOnLoopback: (port: number) => Effect.Effect<boolean>;

  /**
   * Reserve an ephemeral loopback port and release it immediately.
   */
  readonly reserveLoopbackPort: (host?: string) => Effect.Effect<number, NetError>;

  /**
   * Resolve an available listening port, preferring the provided port first.
   */
  readonly findAvailablePort: (preferred: number) => Effect.Effect<number, NetError>;
}

/**
 * NetService - Service tag for startup networking helpers.
 */
export class NetService extends Context.Service<NetService, NetServiceShape>()(
  "@coddy/shared/Net/NetService",
) {}

export const make = () => {
  /**
   * Returns true when a TCP server can bind to {host, port}.
   * `EADDRNOTAVAIL` is treated as available so IPv6-absent hosts don't fail
   * loopback availability checks.
   */
  const canListenOnHost = (port: number, host: string): Effect.Effect<boolean> =>
    Effect.callback<boolean>((resume) => {
      const server = NodeNet.createServer();
      const closeOnce = makeCloseServerOnce(server);
      let settled = false;

      const settle = (value: boolean) => {
        if (settled) return;
        settled = true;
        resume(Effect.succeed(value));
      };

      server.unref();

      server.once("error", (cause) => {
        if (isErrnoExceptionWithCode(cause) && cause.code === "EADDRNOTAVAIL") {
          settle(true);
          return;
        }
        settle(false);
      });

      server.once("listening", () => {
        if (settled) return;
        closeOnce(() => {
          settle(true);
        });
      });

      server.listen({ host, port });

      return Effect.sync(() => {
        settled = true;
        closeServer(closeOnce);
      });
    });

  const hasListenerOnHost = (port: number, host: string): Effect.Effect<boolean> =>
    Effect.callback<boolean>((resume) => {
      const socket = NodeNet.createConnection({ host, port });
      let settled = false;

      const settle = (value: boolean) => {
        if (settled) return;
        settled = true;
        socket.destroy();
        resume(Effect.succeed(value));
      };

      socket.unref();
      socket.setTimeout(250);
      socket.once("connect", () => {
        settle(true);
      });
      socket.once("error", () => {
        settle(false);
      });
      socket.once("timeout", () => {
        settle(false);
      });

      return Effect.sync(() => {
        settled = true;
        socket.destroy();
      });
    });

  const isPortAvailableOnLoopback = (port: number): Effect.Effect<boolean> =>
    Effect.gen(function* () {
      const hasListener = yield* Effect.zipWith(
        hasListenerOnHost(port, "127.0.0.1"),
        hasListenerOnHost(port, "::1"),
        (ipv4, ipv6) => ipv4 || ipv6,
      );
      if (hasListener) {
        return false;
      }

      return yield* Effect.zipWith(
        canListenOnHost(port, "127.0.0.1"),
        canListenOnHost(port, "::1"),
        (ipv4, ipv6) => ipv4 && ipv6,
      );
    });

  /**
   * Reserve an ephemeral loopback port and release it immediately.
   * Returns the reserved port number.
   */
  const reserveLoopbackPort = (host = "127.0.0.1"): Effect.Effect<number, NetError> =>
    Effect.callback<number, NetError>((resume) => {
      const probe = NodeNet.createServer();
      const closeOnce = makeCloseServerOnce(probe);
      let settled = false;

      const settle = (effect: Effect.Effect<number, NetError>) => {
        if (settled) return;
        settled = true;
        resume(effect);
      };

      probe.once("error", (cause) => {
        settle(Effect.fail(new NetError({ message: "Failed to reserve loopback port", cause })));
      });

      probe.listen(0, host, () => {
        if (settled) return;
        const address = probe.address();
        const port = typeof address === "object" && address !== null ? address.port : 0;
        closeOnce(() => {
          if (port > 0) {
            settle(Effect.succeed(port));
            return;
          }
          settle(Effect.fail(new NetError({ message: "Failed to reserve loopback port" })));
        });
      });

      return Effect.sync(() => {
        settled = true;
        closeServer(closeOnce);
      });
    });

  return {
    canListenOnHost,
    isPortAvailableOnLoopback,
    reserveLoopbackPort,
    findAvailablePort: (preferred) =>
      Effect.gen(function* () {
        if (preferred > 0 && (yield* isPortAvailableOnLoopback(preferred))) {
          return preferred;
        }
        return yield* reserveLoopbackPort();
      }),
  } satisfies NetServiceShape;
};

export const layer = Layer.sync(NetService, make);
