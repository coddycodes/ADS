// @effect-diagnostics nodeBuiltinImport:off - The CLI entrypoint regression runs in a child Node process.
import * as NodeChildProcess from "node:child_process";
import * as NodeServices from "@effect/platform-node/NodeServices";
import * as NodeOS from "node:os";
import * as NetService from "@bibcode/shared/Net";
import { HostProcessPlatform } from "@bibcode/shared/hostProcess";
import { assert, describe, it } from "@effect/vitest";
import * as ConfigProvider from "effect/ConfigProvider";
import * as Effect from "effect/Effect";
import * as Fiber from "effect/Fiber";
import * as Layer from "effect/Layer";
import * as Path from "effect/Path";
import * as PlatformError from "effect/PlatformError";
import * as Sink from "effect/Sink";
import * as Stream from "effect/Stream";
import { ChildProcessSpawner } from "effect/process";

import {
  checkPortAvailabilityOnHosts,
  applyDevRunnerRepoEnv,
  createDevRunnerEnv,
  findFirstAvailableOffset,
  getDevRunnerModeArgs,
  resolveModePortOffsets,
  resolveOffset,
  runDevRunnerWithInput,
  runDevRunnerMain,
  DevRunnerPortExhaustedError,
} from "./dev-runner.ts";

const emptyConfigLayer = ConfigProvider.layer(ConfigProvider.fromEnv({ env: {} }));
const netServiceLayer = Layer.succeed(NetService.NetService, {
  canListenOnHost: () => Effect.succeed(true),
  isPortAvailableOnLoopback: () => Effect.succeed(true),
  reserveLoopbackPort: () => Effect.succeed(49_152),
  findAvailablePort: (port) => Effect.succeed(port),
});

function mockProcess(
  exit: number | PlatformError.PlatformError | Effect.Effect<never>,
  onKill: () => void = () => undefined,
) {
  return ChildProcessSpawner.makeHandle({
    pid: ChildProcessSpawner.ProcessId(1),
    exitCode:
      typeof exit === "number"
        ? Effect.succeed(ChildProcessSpawner.ExitCode(exit))
        : Effect.isEffect(exit)
          ? exit
          : Effect.fail(exit),
    isRunning: Effect.succeed(false),
    kill: () => Effect.sync(onKill),
    unref: Effect.succeed(Effect.void),
    stdin: Sink.drain,
    stdout: Stream.empty,
    stderr: Stream.empty,
    all: Stream.empty,
    getInputFd: () => Sink.drain,
    getOutputFd: () => Stream.empty,
  });
}

const devServerInput = {
  mode: "dev:server",
  homeDir: "/tmp/bibcode-dev-runner",
  noBrowser: undefined,
  autoBootstrapProjectFromCwd: undefined,
  logWebSocketEvents: undefined,
  host: undefined,
  port: 13_773,
  devUrl: undefined,
  dryRun: false,
  runArgs: ["--inspect", "secret-token-value"],
} as const;

it.layer(NodeServices.layer)("dev-runner", (it) => {
  describe("getDevRunnerModeArgs", () => {
    it.effect("places Vite+ run flags before the task name", () =>
      Effect.sync(() => {
        assert.deepStrictEqual(getDevRunnerModeArgs("dev"), [
          "run",
          "--filter=@bibcode/contracts",
          "--filter=@bibcode/web",
          "--filter=bibcode",
          "--parallel",
          "dev",
        ]);
      }),
    );
  });

  describe("resolveOffset", () => {
    it.effect("uses explicit BIBCODE_PORT_OFFSET when provided", () =>
      Effect.gen(function* () {
        const result = yield* resolveOffset({ portOffset: 12, devInstance: undefined });
        assert.deepStrictEqual(result, {
          offset: 12,
          source: "BIBCODE_PORT_OFFSET=12",
        });
      }),
    );

    it.effect("hashes non-numeric instance values", () =>
      Effect.gen(function* () {
        const result = yield* resolveOffset({
          portOffset: undefined,
          devInstance: "feature-branch",
        });
        assert.ok(result.offset >= 1);
        assert.ok(result.offset <= 3000);
      }),
    );

    it.effect("uses default and numeric instance offsets", () =>
      Effect.gen(function* () {
        assert.deepStrictEqual(
          yield* resolveOffset({ portOffset: undefined, devInstance: "   " }),
          { offset: 0, source: "default ports" },
        );
        assert.deepStrictEqual(
          yield* resolveOffset({ portOffset: undefined, devInstance: " 42 " }),
          { offset: 42, source: "numeric BIBCODE_DEV_INSTANCE=42" },
        );
      }),
    );

    it.effect("returns structured context for a negative port offset", () =>
      Effect.gen(function* () {
        const error = yield* resolveOffset({ portOffset: -1, devInstance: undefined }).pipe(
          Effect.flip,
        );

        assert.equal(error._tag, "DevRunnerInvalidPortOffsetError");
        assert.equal(error.configKey, "BIBCODE_PORT_OFFSET");
        assert.equal(error.portOffset, -1);
        assert.equal(error.minimum, 0);
        assert.include(error.message, "must be at least 0");
        assert.ok(!("cause" in error));
      }),
    );
  });

  describe("createDevRunnerEnv", () => {
    it.effect("disables Claude keychain access by default in every dev mode", () =>
      Effect.gen(function* () {
        for (const mode of ["dev", "dev:server", "dev:web"] as const) {
          const env = yield* createDevRunnerEnv({
            mode,
            baseEnv: {},
            serverOffset: 0,
            webOffset: 0,
            homeDir: undefined,
            noBrowser: undefined,
            autoBootstrapProjectFromCwd: undefined,
            logWebSocketEvents: undefined,
            host: undefined,
            port: undefined,
            devUrl: undefined,
          });

          assert.equal(env.BIBCODE_CLAUDE_KEYCHAIN_ACCESS, "disabled");
        }
      }),
    );

    it.effect("preserves an inherited Claude keychain opt-in", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {
            BIBCODE_CLAUDE_KEYCHAIN_ACCESS: "enabled",
          },
          serverOffset: 0,
          webOffset: 0,
          homeDir: undefined,
          noBrowser: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.BIBCODE_CLAUDE_KEYCHAIN_ACCESS, "enabled");
      }),
    );

    it.effect("defaults BIBCODE_HOME to ~/.bibcode when not provided", () =>
      Effect.gen(function* () {
        const path = yield* Path.Path;
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {},
          serverOffset: 0,
          webOffset: 0,
          homeDir: undefined,
          noBrowser: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.BIBCODE_HOME, path.resolve(NodeOS.homedir(), ".bibcode"));
      }),
    );

    it.effect("supports explicit typed overrides", () =>
      Effect.gen(function* () {
        const path = yield* Path.Path;
        const env = yield* createDevRunnerEnv({
          mode: "dev:server",
          baseEnv: {},
          serverOffset: 0,
          webOffset: 0,
          homeDir: "/tmp/custom-bibcode",
          noBrowser: true,
          autoBootstrapProjectFromCwd: false,
          logWebSocketEvents: true,
          host: "0.0.0.0",
          port: 4222,
          devUrl: new URL("http://localhost:7331"),
        });

        assert.equal(env.BIBCODE_HOME, path.resolve("/tmp/custom-bibcode"));
        assert.equal(env.BIBCODE_PORT, "4222");
        assert.equal(env.VITE_HTTP_URL, "http://localhost:4222");
        assert.equal(env.VITE_WS_URL, "ws://localhost:4222");
        assert.equal(env.BIBCODE_NO_BROWSER, "true");
        assert.equal(env.BIBCODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD, "0");
        assert.equal(env.BIBCODE_LOG_WS_EVENTS, "1");
        assert.equal(env.BIBCODE_HOST, "0.0.0.0");
        assert.equal(env.VITE_DEV_SERVER_URL, "http://localhost:7331/");
      }),
    );

    it.effect("does not force websocket logging on in dev mode when unset", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {
            BIBCODE_LOG_WS_EVENTS: "keep-me-out",
          },
          serverOffset: 0,
          webOffset: 0,
          homeDir: undefined,
          noBrowser: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.BIBCODE_MODE, "web");
        assert.equal(env.BIBCODE_LOG_WS_EVENTS, undefined);
      }),
    );

    it.effect("forwards explicit websocket logging false without coercing it away", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {
            BIBCODE_LOG_WS_EVENTS: "1",
          },
          serverOffset: 0,
          webOffset: 0,
          homeDir: undefined,
          noBrowser: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: false,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.BIBCODE_LOG_WS_EVENTS, "0");
      }),
    );

    it.effect(
      "forwards true bootstrap and false browser flags while clearing desktop routing",
      () =>
        Effect.gen(function* () {
          const env = yield* createDevRunnerEnv({
            mode: "dev:web",
            baseEnv: { BIBCODE_DESKTOP_WS_URL: "ws://desktop", BIBCODE_NO_BROWSER: "stale" },
            serverOffset: 2,
            webOffset: 3,
            homeDir: undefined,
            noBrowser: false,
            autoBootstrapProjectFromCwd: true,
            logWebSocketEvents: undefined,
            host: undefined,
            port: undefined,
            devUrl: undefined,
          });
          assert.equal(env.BIBCODE_NO_BROWSER, "false");
          assert.equal(env.BIBCODE_AUTO_BOOTSTRAP_PROJECT_FROM_CWD, "1");
          assert.equal(env.BIBCODE_DESKTOP_WS_URL, undefined);
          assert.equal(env.PORT, "5736");
        }),
    );

    it.effect("uses custom homeDir when provided", () =>
      Effect.gen(function* () {
        const path = yield* Path.Path;
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {},
          serverOffset: 0,
          webOffset: 0,
          homeDir: "/tmp/my-bibcode",
          noBrowser: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.BIBCODE_HOME, path.resolve("/tmp/my-bibcode"));
      }),
    );

    it.effect("defaults dev server mode to the higher backend port range", () =>
      Effect.gen(function* () {
        const env = yield* createDevRunnerEnv({
          mode: "dev",
          baseEnv: {},
          serverOffset: 0,
          webOffset: 0,
          homeDir: undefined,
          noBrowser: undefined,
          autoBootstrapProjectFromCwd: undefined,
          logWebSocketEvents: undefined,
          host: undefined,
          port: undefined,
          devUrl: undefined,
        });

        assert.equal(env.BIBCODE_PORT, "13773");
        assert.equal(env.VITE_HTTP_URL, "http://localhost:13773");
        assert.equal(env.VITE_WS_URL, "ws://localhost:13773");
      }),
    );
  });

  describe("findFirstAvailableOffset", () => {
    it.effect("returns the starting offset when required ports are available", () =>
      Effect.gen(function* () {
        const offset = yield* findFirstAvailableOffset({
          startOffset: 0,
          requireServerPort: true,
          requireWebPort: true,
          checkPortAvailability: () => Effect.succeed(true),
        });

        assert.equal(offset, 0);
      }),
    );

    it.effect("advances until all required ports are available", () =>
      Effect.gen(function* () {
        const taken = new Set([13773, 5733, 13774, 5734]);
        const offset = yield* findFirstAvailableOffset({
          startOffset: 0,
          requireServerPort: true,
          requireWebPort: true,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.equal(offset, 2);
      }),
    );

    it.effect("allows offsets where the non-required server port exceeds max", () =>
      Effect.gen(function* () {
        const offset = yield* findFirstAvailableOffset({
          startOffset: 59_802,
          requireServerPort: false,
          requireWebPort: true,
          checkPortAvailability: () => Effect.succeed(true),
        });

        assert.equal(offset, 59_802);
      }),
    );

    it.effect("reports the exhausted range and required port set", () =>
      Effect.gen(function* () {
        const error = yield* findFirstAvailableOffset({
          startOffset: 51_763,
          requireServerPort: true,
          requireWebPort: false,
          checkPortAvailability: () => Effect.succeed(true),
        }).pipe(Effect.flip);

        if (error._tag !== "DevRunnerPortExhaustedError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.startOffset, 51_763);
        assert.equal(error.requireServerPort, true);
        assert.equal(error.requireWebPort, false);
        assert.equal(error.baseServerPort, 13_773);
        assert.equal(error.baseWebPort, 5_733);
        assert.equal(error.maximumPort, 65_535);
        assert.include(error.message, "No required dev ports were available");
        assert.instanceOf(error, DevRunnerPortExhaustedError);
        assert.ok(!("cause" in error));
      }),
    );

    it.effect("returns immediately when no ports are required", () =>
      Effect.gen(function* () {
        const offset = yield* findFirstAvailableOffset({
          startOffset: 5,
          requireServerPort: false,
          requireWebPort: false,
          checkPortAvailability: () => Effect.succeed(false),
        });
        assert.equal(offset, 5);
      }),
    );

    it.effect("uses the default network service and stops after the first unavailable host", () =>
      Effect.gen(function* () {
        const hosts: string[] = [];
        const layer = Layer.succeed(NetService.NetService, {
          canListenOnHost: (port: number, host: string) => {
            hosts.push(host);
            return Effect.succeed(port !== 13_773 || host !== "0.0.0.0");
          },
          isPortAvailableOnLoopback: () => Effect.succeed(true),
          reserveLoopbackPort: () => Effect.succeed(49_152),
          findAvailablePort: (port: number) => Effect.succeed(port),
        } as never);
        const offset = yield* findFirstAvailableOffset({
          startOffset: 0,
          requireServerPort: true,
          requireWebPort: false,
        }).pipe(Effect.provide(layer));
        assert.equal(offset, 1);
        assert.deepStrictEqual(hosts.slice(0, 2), ["127.0.0.1", "0.0.0.0"]);
      }),
    );
  });

  describe("checkPortAvailabilityOnHosts", () => {
    it.effect("checks overlapping hosts sequentially to avoid self-interference", () =>
      Effect.gen(function* () {
        let inFlightCount = 0;
        const calls: Array<[number, string]> = [];

        const available = yield* checkPortAvailabilityOnHosts(
          13_773,
          ["127.0.0.1", "0.0.0.0", "::"],
          (port, host) =>
            Effect.promise(async () => {
              calls.push([port, host]);
              inFlightCount += 1;
              const overlapped = inFlightCount > 1;
              await Promise.resolve();
              inFlightCount -= 1;
              return !overlapped;
            }),
        );

        assert.equal(available, true);
        assert.deepStrictEqual(calls, [
          [13_773, "127.0.0.1"],
          [13_773, "0.0.0.0"],
          [13_773, "::"],
        ]);
      }),
    );

    it.effect("stops probing after the first unavailable host", () =>
      Effect.gen(function* () {
        const calls: string[] = [];
        const available = yield* checkPortAvailabilityOnHosts(
          13_773,
          ["127.0.0.1", "0.0.0.0", "::1"],
          (_port, host) => {
            calls.push(host);
            return Effect.succeed(host !== "0.0.0.0");
          },
        );
        assert.equal(available, false);
        assert.deepStrictEqual(calls, ["127.0.0.1", "0.0.0.0"]);
      }),
    );
  });

  describe("resolveModePortOffsets", () => {
    it.effect("uses a shared fallback offset for dev mode", () =>
      Effect.gen(function* () {
        const taken = new Set([13773, 5733]);
        const offsets = yield* resolveModePortOffsets({
          mode: "dev",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: false,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 1, webOffset: 1 });
      }),
    );

    it.effect("keeps server offset stable for dev:web and only shifts web offset", () =>
      Effect.gen(function* () {
        const taken = new Set([5733]);
        const offsets = yield* resolveModePortOffsets({
          mode: "dev:web",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: false,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 0, webOffset: 1 });
      }),
    );

    it.effect("shifts only server offset for dev:server", () =>
      Effect.gen(function* () {
        const taken = new Set([13773]);
        const offsets = yield* resolveModePortOffsets({
          mode: "dev:server",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: false,
          checkPortAvailability: (port) => Effect.succeed(!taken.has(port)),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 1, webOffset: 1 });
      }),
    );

    it.effect("respects explicit dev-url override for dev:web", () =>
      Effect.gen(function* () {
        const offsets = yield* resolveModePortOffsets({
          mode: "dev:web",
          startOffset: 0,
          hasExplicitServerPort: false,
          hasExplicitDevUrl: true,
          checkPortAvailability: () => Effect.succeed(false),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 0, webOffset: 0 });
      }),
    );

    it.effect("respects explicit server port override for dev:server", () =>
      Effect.gen(function* () {
        const offsets = yield* resolveModePortOffsets({
          mode: "dev:server",
          startOffset: 0,
          hasExplicitServerPort: true,
          hasExplicitDevUrl: false,
          checkPortAvailability: () => Effect.succeed(false),
        });

        assert.deepStrictEqual(offsets, { serverOffset: 0, webOffset: 0 });
      }),
    );
  });

  describe("runDevRunnerWithInput", () => {
    it.effect("completes dry runs without spawning and records shifted port selection", () => {
      let spawnCount = 0;
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => {
          spawnCount += 1;
          return Effect.succeed(mockProcess(0));
        }),
      );
      const shiftingNet = Layer.succeed(NetService.NetService, {
        canListenOnHost: (port: number) => Effect.succeed(port !== 13_773 && port !== 5_733),
        isPortAvailableOnLoopback: () => Effect.succeed(true),
        reserveLoopbackPort: () => Effect.succeed(49_152),
        findAvailablePort: (port: number) => Effect.succeed(port),
      } as never);

      return runDevRunnerWithInput({
        ...devServerInput,
        mode: "dev",
        port: undefined,
        dryRun: true,
      }).pipe(
        Effect.provide(Layer.mergeAll(emptyConfigLayer, shiftingNet, spawnerLayer)),
        Effect.provideService(HostProcessPlatform, "linux"),
        Effect.tap(() => Effect.sync(() => assert.equal(spawnCount, 0))),
      );
    });

    it.effect("accepts zero exits from the Vite+ child", () => {
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.succeed(mockProcess(0))),
      );
      return runDevRunnerWithInput(devServerInput).pipe(
        Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
        Effect.provideService(HostProcessPlatform, "linux"),
      );
    });

    it.effect("kills the scoped child when the runner is interrupted", () =>
      Effect.gen(function* () {
        let killCount = 0;
        const spawnerLayer = Layer.succeed(
          ChildProcessSpawner.ChildProcessSpawner,
          ChildProcessSpawner.make(() =>
            Effect.succeed(mockProcess(Effect.never, () => (killCount += 1))),
          ),
        );
        const fiber = yield* runDevRunnerWithInput(devServerInput).pipe(
          Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
          Effect.provideService(HostProcessPlatform, "linux"),
          Effect.forkChild({ startImmediately: true }),
        );
        yield* Effect.yieldNow;
        yield* Fiber.interrupt(fiber);
        assert.equal(killCount, 1);
      }),
    );

    it.effect("preserves invalid configuration as the exact cause", () =>
      Effect.gen(function* () {
        const error = yield* runDevRunnerWithInput({ ...devServerInput, dryRun: true }).pipe(
          Effect.provide(
            Layer.merge(
              netServiceLayer,
              ConfigProvider.layer(
                ConfigProvider.fromEnv({ env: { BIBCODE_PORT_OFFSET: "not-an-integer" } }),
              ),
            ),
          ),
          Effect.flip,
        );

        if (error._tag !== "DevRunnerConfigurationError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.deepStrictEqual(error.configKeys, ["BIBCODE_PORT_OFFSET", "BIBCODE_DEV_INSTANCE"]);
        assert.ok(error.cause !== undefined);
        assert.ok(!error.message.includes(String((error.cause as Error).message)));
      }),
    );

    it.effect("preserves process spawn context and the exact platform cause", () => {
      const cause = PlatformError.systemError({
        _tag: "NotFound",
        module: "ChildProcess",
        method: "spawn",
        description: "vp was not found",
      });
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.fail(cause)),
      );

      return Effect.gen(function* () {
        const error = yield* runDevRunnerWithInput(devServerInput).pipe(
          Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
          Effect.provideService(HostProcessPlatform, "linux"),
          Effect.flip,
        );

        if (error._tag !== "DevRunnerProcessError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.operation, "spawn");
        assert.equal(error.mode, "dev:server");
        assert.equal(error.executable, "vp");
        assert.equal(error.argumentCount, 5);
        assert.equal(error.shell, false);
        assert.equal(error.cause, cause);
        assert.ok(!error.message.includes(cause.message));
        assert.notProperty(error, "args");
        assert.notInclude(error.message, "secret-token-value");
      });
    });

    it.effect("reports non-zero exits without manufacturing a cause", () => {
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.succeed(mockProcess(17))),
      );

      return Effect.gen(function* () {
        const error = yield* runDevRunnerWithInput(devServerInput).pipe(
          Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
          Effect.provideService(HostProcessPlatform, "linux"),
          Effect.flip,
        );

        if (error._tag !== "DevRunnerProcessExitError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.mode, "dev:server");
        assert.equal(error.executable, "vp");
        assert.equal(error.argumentCount, 5);
        assert.equal(error.shell, false);
        assert.equal(error.exitCode, 17);
        assert.ok(!("cause" in error));
        assert.notProperty(error, "args");
        assert.notInclude(error.message, "secret-token-value");
      });
    });

    it.effect("preserves wait-for-exit failures as the exact cause", () => {
      const cause = PlatformError.systemError({
        _tag: "Unknown",
        module: "ChildProcess",
        method: "exitCode",
        description: "process status became unavailable",
      });
      const spawnerLayer = Layer.succeed(
        ChildProcessSpawner.ChildProcessSpawner,
        ChildProcessSpawner.make(() => Effect.succeed(mockProcess(cause))),
      );

      return Effect.gen(function* () {
        const error = yield* runDevRunnerWithInput(devServerInput).pipe(
          Effect.provide(Layer.mergeAll(emptyConfigLayer, netServiceLayer, spawnerLayer)),
          Effect.provideService(HostProcessPlatform, "linux"),
          Effect.flip,
        );

        if (error._tag !== "DevRunnerProcessError") {
          assert.fail(`Unexpected error: ${error._tag}`);
        }
        assert.equal(error.operation, "wait-for-exit");
        assert.equal(error.mode, "dev:server");
        assert.equal(error.executable, "vp");
        assert.equal(error.argumentCount, 5);
        assert.equal(error.shell, false);
        assert.equal(error.cause, cause);
        assert.ok(!error.message.includes(cause.message));
        assert.notProperty(error, "args");
        assert.notInclude(error.message, "secret-token-value");
      });
    });
  });
});

it("applies repository environment only for the direct CLI entrypoint", () => {
  const key = "BIBCODE_DEV_RUNNER_TEST_ENV";
  const previous = process.env[key];
  const launched: unknown[] = [];
  const applied: Array<Readonly<Record<string, string | undefined>>> = [];
  try {
    applyDevRunnerRepoEnv({ [key]: "configured" });
    assert.equal(process.env[key], "configured");
  } finally {
    if (previous === undefined) delete process.env[key];
    else process.env[key] = previous;
  }

  assert.equal(
    runDevRunnerMain(
      false,
      (effect) => launched.push(effect),
      { TEST: "1" },
      (env) => applied.push(env),
    ),
    false,
  );
  assert.equal(
    runDevRunnerMain(
      true,
      (effect) => launched.push(effect),
      { TEST: "1" },
      (env) => applied.push(env),
    ),
    true,
  );
  assert.equal(launched.length, 1);
  assert.deepStrictEqual(applied, [{ TEST: "1" }]);
});

it("does not stringify unset repository configuration", () => {
  const keys = ["BIBCODE_PORT", "BIBCODE_PORT_OFFSET"] as const;
  const previous = Object.fromEntries(keys.map((key) => [key, process.env[key]]));
  try {
    for (const key of keys) delete process.env[key];
    applyDevRunnerRepoEnv({ BIBCODE_PORT: undefined, BIBCODE_PORT_OFFSET: undefined });
    for (const key of keys) assert.equal(process.env[key], undefined);
  } finally {
    for (const key of keys) {
      if (previous[key] === undefined) delete process.env[key];
      else process.env[key] = previous[key];
    }
  }
});

it("runs a dry dev CLI without a configured server port", () => {
  const env = { ...process.env };
  Reflect.deleteProperty(env, "BIBCODE_PORT");
  Reflect.deleteProperty(env, "PORT");

  const result = NodeChildProcess.spawnSync(
    process.execPath,
    ["scripts/dev-runner.ts", "dev", "--dry-run"],
    {
      cwd: new URL("..", import.meta.url),
      encoding: "utf8",
      env,
    },
  );

  assert.equal(result.status, 0, result.stderr);
  assert.include(result.stdout, "source=default ports");
});
