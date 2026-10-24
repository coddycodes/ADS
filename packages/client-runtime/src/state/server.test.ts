import {
  EnvironmentId,
  ProviderInstanceId,
  type ServerConfig,
  type ServerLifecycleWelcomePayload,
  WS_METHODS,
} from "@bibcode/contracts";
import { describe, expect, it } from "@effect/vitest";
import * as DateTime from "effect/DateTime";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Latch from "effect/Latch";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import { vi } from "vite-plus/test";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";

import {
  applyServerConfigProjection,
  createServerEnvironmentAtoms,
  projectServerWelcome,
} from "./server.ts";
import { EnvironmentRegistry } from "../connection/registry.ts";
import { EnvironmentSupervisor } from "../connection/supervisor.ts";

const CONFIG = {
  availableEditors: [],
  issues: [],
  keybindings: {},
  keybindingsConfigPath: null,
  observability: null,
  providers: [],
  settings: {},
} as unknown as ServerConfig;

describe("server state projection", () => {
  it("applies every config category to the projected snapshot", () => {
    const snapshot = applyServerConfigProjection(Option.none(), {
      version: 1,
      type: "snapshot",
      config: CONFIG,
    });
    const settings = { ...CONFIG.settings };
    const projected = applyServerConfigProjection(snapshot, {
      version: 1,
      type: "settingsUpdated",
      payload: { settings },
    });

    const result = Option.getOrThrow(projected);
    expect(result.config.settings).toBe(settings);
    expect(result.latestEvent.type).toBe("settingsUpdated");
  });

  it("retains welcome when a ready event follows in the same stream chunk", () => {
    const welcome = {
      environment: {} as ServerLifecycleWelcomePayload["environment"],
      cwd: "/repo",
      projectName: "repo",
    } as ServerLifecycleWelcomePayload;
    const [afterWelcome] = projectServerWelcome(Option.none(), {
      type: "welcome",
      payload: welcome,
    });
    const [afterReady, emitted] = projectServerWelcome(afterWelcome, {
      type: "ready",
      payload: {},
    });

    expect(Option.getOrThrow(afterReady)).toBe(welcome);
    expect(emitted).toEqual([]);
  });
});

describe("server provider usage commands", () => {
  it.effect("sends reset requests to the selected environment and returns the decoded result", () =>
    Effect.gen(function* () {
      const environmentId = EnvironmentId.make("environment-reset");
      const calls: Array<{ readonly method: string; readonly input: unknown }> = [];
      const expected = {
        outcome: "reset" as const,
        usage: {
          readAt: DateTime.makeUnsafe("2026-07-22T12:00:00.000Z"),
          isFetching: false,
          providers: [],
        },
      };
      const session = yield* SubscriptionRef.make(
        Option.some({
          client: {
            [WS_METHODS.serverConsumeCodexRateLimitReset]: (input: unknown) =>
              Effect.sync(() => {
                calls.push({ method: WS_METHODS.serverConsumeCodexRateLimitReset, input });
                return expected;
              }),
          },
        } as never),
      );
      const supervisor = EnvironmentSupervisor.of({
        target: { environmentId, label: "Reset environment" },
        session,
      } as never);
      const selectedEnvironments: string[] = [];
      const run: EnvironmentRegistry["Service"]["run"] = (selectedEnvironmentId, effect) => {
        selectedEnvironments.push(selectedEnvironmentId);
        return Effect.provideService(effect, EnvironmentSupervisor, supervisor);
      };
      const environmentRegistry = EnvironmentRegistry.of({
        run,
      } as never);
      const atoms = createServerEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
        { initialConfigValueAtom: () => Atom.make(null) },
      );
      const command = atoms.consumeCodexRateLimitReset;
      expect(command).toBeDefined();
      const atomRegistry = AtomRegistry.make();

      const result = yield* Effect.promise(() =>
        command.run(atomRegistry, {
          environmentId,
          input: { requestId: "request-123" },
        }),
      );

      expect(selectedEnvironments).toEqual([environmentId]);
      expect(calls).toEqual([
        {
          method: "server.consumeCodexRateLimitReset",
          input: { requestId: "request-123" },
        },
      ]);
      expect(result).toMatchObject({ _tag: "Success", value: expected, waiting: false });
      atomRegistry.dispose();
    }),
  );

  it.effect("shares one active reset request within the same environment", () =>
    Effect.gen(function* () {
      const environmentId = EnvironmentId.make("environment-single-flight");
      const latch = Latch.makeUnsafe();
      let executions = 0;
      const expected = {
        outcome: "nothingToReset" as const,
        usage: {
          readAt: DateTime.makeUnsafe("2026-07-22T12:00:00.000Z"),
          isFetching: false,
          providers: [],
        },
      };
      const session = yield* SubscriptionRef.make(
        Option.some({
          client: {
            [WS_METHODS.serverConsumeCodexRateLimitReset]: () =>
              Effect.sync(() => {
                executions += 1;
              }).pipe(Effect.andThen(latch.await), Effect.as(expected)),
          },
        } as never),
      );
      const supervisor = EnvironmentSupervisor.of({
        target: { environmentId, label: "Single-flight environment" },
        session,
      } as never);
      const run: EnvironmentRegistry["Service"]["run"] = (_selectedEnvironmentId, effect) =>
        Effect.provideService(effect, EnvironmentSupervisor, supervisor);
      const environmentRegistry = EnvironmentRegistry.of({
        run,
      } as never);
      const atoms = createServerEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
        { initialConfigValueAtom: () => Atom.make(null) },
      );
      const atomRegistry = AtomRegistry.make();

      const first = atoms.consumeCodexRateLimitReset.run(atomRegistry, {
        environmentId,
        input: { requestId: "request-1" },
      });
      const second = atoms.consumeCodexRateLimitReset.run(atomRegistry, {
        environmentId,
        input: { requestId: "request-2" },
      });
      yield* Effect.yieldNow;
      latch.openUnsafe();
      const results = yield* Effect.promise(() => Promise.all([first, second]));

      expect(executions).toBe(1);
      expect(results).toEqual([
        expect.objectContaining({ _tag: "Success", value: expected }),
        expect.objectContaining({ _tag: "Success", value: expected }),
      ]);
      atomRegistry.dispose();
    }),
  );
});

describe("workspace provider capabilities", () => {
  it.effect("shares a context and isolates concurrent workspaces and configuration revisions", () =>
    Effect.gen(function* () {
      const environmentId = EnvironmentId.make("skills");
      const calls: unknown[] = [];
      const firstGate = Latch.makeUnsafe();
      const connection = yield* SubscriptionRef.make({ phase: "connected", generation: 1 });
      const supervisor = EnvironmentSupervisor.of({
        target: { environmentId, label: "Skills" },
        state: connection,
        session: yield* SubscriptionRef.make(
          Option.some({
            client: {
              [WS_METHODS.serverGetProviderCapabilities]: (input: { cwd: string }) =>
                Effect.gen(function* () {
                  calls.push(input);
                  if (input.cwd === "/first") yield* firstGate.await;
                  return {
                    slashCommands: [],
                    agents: [],
                    issues: [],
                    skills: [
                      {
                        name: "personal",
                        path: "/home/skills/personal/SKILL.md",
                        enabled: true,
                        invocation: "dollar",
                      },
                      {
                        name: input.cwd,
                        path: `${input.cwd}/SKILL.md`,
                        enabled: true,
                        invocation: "dollar",
                      },
                    ],
                  };
                }),
            },
          } as never),
        ),
      } as never);
      const environment = EnvironmentRegistry.of({
        run: <A, E>(_: EnvironmentId, effect: Effect.Effect<A, E, EnvironmentSupervisor>) =>
          Effect.provideService(effect, EnvironmentSupervisor, supervisor),
        followStream: <A, E>(
          _: EnvironmentId,
          stream: Stream.Stream<A, E, EnvironmentSupervisor>,
        ) => Stream.provideService(stream, EnvironmentSupervisor, supervisor),
      } as never);
      const atoms = createServerEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environment)),
        { initialConfigValueAtom: () => Atom.make(null) },
      );
      const registry = AtomRegistry.make();
      const target = {
        environmentId,
        input: { instanceId: ProviderInstanceId.make("codex"), cwd: "/first", revision: 1 },
      };
      const first = atoms.providerCapabilities(target);
      expect(atoms.providerCapabilities({ ...target })).toBe(first);
      const second = atoms.providerCapabilities({
        ...target,
        input: { ...target.input, cwd: "/second" },
      });
      registry.mount(first);
      registry.mount(second);
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(registry.get(second))).toBe(true)),
      );
      expect(AsyncResult.value(registry.get(first))).toEqual(Option.none());
      yield* firstGate.open;
      yield* Effect.promise(() =>
        vi.waitFor(() => expect(AsyncResult.isSuccess(registry.get(first))).toBe(true)),
      );
      expect(Option.getOrThrow(AsyncResult.value(registry.get(second))).skills[1]?.name).toBe(
        "/second",
      );
      expect(calls).toEqual(
        expect.arrayContaining([
          { instanceId: "codex", cwd: "/first" },
          { instanceId: "codex", cwd: "/second" },
        ]),
      );
      expect(calls).toHaveLength(2);
      const changed = atoms.providerCapabilities({
        ...target,
        input: { ...target.input, revision: 2 },
      });
      expect(changed).not.toBe(first);
      expect(
        atoms.providerCapabilities({ ...target, environmentId: EnvironmentId.make("remote") }),
      ).not.toBe(first);
      expect(
        atoms.providerCapabilities({
          ...target,
          input: { ...target.input, instanceId: ProviderInstanceId.make("codex-other") },
        }),
      ).not.toBe(first);
      registry.mount(changed);
      yield* Effect.promise(() => vi.waitFor(() => expect(calls).toHaveLength(3)));
      registry.dispose();
    }),
  );

  it("invalidates skills even when secret settings stay redacted", () => {
    const initial = applyServerConfigProjection(Option.none(), {
      version: 1,
      type: "snapshot",
      config: CONFIG,
    });
    const changed = applyServerConfigProjection(initial, {
      version: 1,
      type: "settingsUpdated",
      payload: { settings: CONFIG.settings },
    });
    expect(Option.getOrThrow(changed).capabilitiesRevision).toBeGreaterThan(
      Option.getOrThrow(initial).capabilitiesRevision,
    );
  });

  // Every reconnect re-sends the snapshot. A new revision is a new capabilities key, and the
  // reconnect already re-reads the mounted one, so an unchanged snapshot must keep the revision.
  // It must also keep the projected config, so nothing that reads it re-renders with a copy.
  it("keeps the projection when a reconnect re-sends an unchanged snapshot", () => {
    const initial = applyServerConfigProjection(Option.none(), {
      version: 1,
      type: "snapshot",
      config: CONFIG,
    });
    const resent = applyServerConfigProjection(initial, {
      version: 1,
      type: "snapshot",
      config: structuredClone(CONFIG),
    });
    expect(Option.getOrThrow(resent)).toBe(Option.getOrThrow(initial));
    const keybindingsOnly = applyServerConfigProjection(resent, {
      version: 1,
      type: "snapshot",
      config: { ...CONFIG, keybindingsConfigPath: "/elsewhere/keybindings.json" },
    } as never);
    expect(Option.getOrThrow(keybindingsOnly).config.keybindingsConfigPath).toBe(
      "/elsewhere/keybindings.json",
    );
    expect(Option.getOrThrow(keybindingsOnly).capabilitiesRevision).toBe(
      Option.getOrThrow(initial).capabilitiesRevision,
    );
    const changed = applyServerConfigProjection(resent, {
      version: 1,
      type: "snapshot",
      config: { ...CONFIG, settings: { ...CONFIG.settings, enableAssistantStreaming: true } },
    } as never);
    expect(Option.getOrThrow(changed).capabilitiesRevision).toBeGreaterThan(
      Option.getOrThrow(resent).capabilitiesRevision,
    );
  });
});

describe("server profiler status query", () => {
  it.effect("reads the status from the selected environment", () =>
    Effect.gen(function* () {
      const environmentId = EnvironmentId.make("environment-profiler");
      const calls: unknown[] = [];
      const status = {
        enabled: true,
        recordingSince: "2026-10-09T12:00:00.000Z",
        diskBytes: 1024,
        eventCount: 3,
        droppedEvents: 0,
        lastCrashAt: null,
        capabilities: { nativeDumps: "available", osCrashRecords: "available", storage: "ok" },
        findings: [],
      };
      const networkHop = Effect.promise(
        () => new Promise<void>((resolve) => setImmediate(resolve)),
      );
      const session = yield* SubscriptionRef.make(
        Option.some({
          client: {
            [WS_METHODS.serverGetProfilerStatus]: (input: unknown) =>
              networkHop.pipe(
                Effect.andThen(
                  Effect.sync(() => {
                    calls.push(input);
                    return status;
                  }),
                ),
              ),
          },
        } as never),
      );
      const supervisor = EnvironmentSupervisor.of({
        target: { environmentId, label: "Profiler environment" },
        session,
        state: yield* SubscriptionRef.make({ phase: "connected", generation: 1 }),
      } as never);
      const selectedEnvironments: string[] = [];
      const environmentRegistry = EnvironmentRegistry.of({
        run: <A, E>(
          selectedEnvironmentId: string,
          effect: Effect.Effect<A, E, EnvironmentSupervisor>,
        ) => {
          selectedEnvironments.push(selectedEnvironmentId);
          return Effect.provideService(effect, EnvironmentSupervisor, supervisor);
        },
        followStream: <A, E>(_: string, stream: Stream.Stream<A, E, EnvironmentSupervisor>) =>
          Stream.provideService(stream, EnvironmentSupervisor, supervisor),
      } as never);
      const atoms = createServerEnvironmentAtoms(
        Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
        { initialConfigValueAtom: () => Atom.make(null) },
      );
      const atomRegistry = AtomRegistry.make();
      const atom = atoms.profilerStatus({ environmentId, input: {} });
      const release = atomRegistry.mount(atom);

      yield* Effect.promise(async () => {
        for (let turn = 0; turn < 50; turn += 1) {
          await new Promise<void>((resolve) => setImmediate(resolve));
        }
      });

      expect(Option.getOrNull(AsyncResult.value(atomRegistry.get(atom)))).toEqual(status);
      expect(selectedEnvironments).toContain(environmentId);
      expect(calls[0]).toEqual({});
      expect(atoms.profilerStatus({ environmentId, input: {} })).toBe(atom);
      release();
      atomRegistry.dispose();
    }),
  );
});
