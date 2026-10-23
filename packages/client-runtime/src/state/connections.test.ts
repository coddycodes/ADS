import { EnvironmentId } from "@bibcode/contracts";
import { it } from "@effect/vitest";
import * as Effect from "effect/Effect";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { beforeEach, describe, expect, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  scheduler: { name: "scheduler" },
  commandConfigs: [] as Array<Record<string, unknown>>,
  followStream: vi.fn((_environmentId: unknown, stream: unknown) => stream),
}));

vi.mock("./runtime.ts", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./runtime.ts")>();
  return {
    ...actual,
    createAtomCommandScheduler: () => harness.scheduler,
    createRuntimeCommand: (_runtime: unknown, config: Record<string, unknown>) => {
      harness.commandConfigs.push(config);
      return config;
    },
    followStreamInEnvironment: harness.followStream,
  };
});

import { EnvironmentRegistry } from "../connection/registry.ts";
import { AVAILABLE_CONNECTION_STATE } from "../connection/model.ts";
import { createEnvironmentCatalogAtoms, EMPTY_ENVIRONMENT_CATALOG_STATE } from "./connections.ts";

beforeEach(() => {
  harness.commandConfigs.length = 0;
  harness.followStream.mockClear();
});

describe("createEnvironmentCatalogAtoms", () => {
  it("projects catalog and network values with initial fallbacks", () => {
    const runtimeAtoms: Array<Atom.Writable<never, never>> = [];
    const runtime = {
      atom: vi.fn((_stream: unknown, options: { initialValue: unknown }) => {
        const atom = Atom.make(AsyncResult.success(options.initialValue));
        runtimeAtoms.push(atom as never);
        return atom;
      }),
    } as never;
    const atoms = createEnvironmentCatalogAtoms(runtime);
    const registry = AtomRegistry.make();

    expect(registry.get(atoms.catalogValueAtom)).toBe(EMPTY_ENVIRONMENT_CATALOG_STATE);
    expect(registry.get(atoms.networkStatusValueAtom)).toBe("unknown");

    registry.set(runtimeAtoms[0]!, AsyncResult.initial(false) as never);
    registry.set(runtimeAtoms[1]!, AsyncResult.initial(false) as never);
    expect(registry.get(atoms.catalogValueAtom)).toBe(EMPTY_ENVIRONMENT_CATALOG_STATE);
    expect(registry.get(atoms.networkStatusValueAtom)).toBe("unknown");
    registry.dispose();
  });

  it.effect("creates environment state atoms and serial catalog commands", () =>
    Effect.gen(function* () {
      const runtime = {
        atom: vi.fn((_stream: unknown, options: { initialValue: unknown }) =>
          Atom.make(AsyncResult.success(options.initialValue)),
        ),
      } as never;
      const atoms = createEnvironmentCatalogAtoms(runtime);
      const environmentId = EnvironmentId.make("env-1");
      const registry = AtomRegistry.make();

      expect(AsyncResult.value(registry.get(atoms.stateAtom(environmentId)))).toEqual(
        expect.objectContaining({ _tag: "Some", value: AVAILABLE_CONNECTION_STATE }),
      );
      expect(harness.followStream).toHaveBeenCalledWith(environmentId, expect.anything());
      expect(harness.commandConfigs).toHaveLength(8);
      for (const config of harness.commandConfigs) {
        expect(config.scheduler).toBe(harness.scheduler);
        expect(config.concurrency).toMatchObject({ mode: "serial" });
        expect((config.concurrency as { key: (input: unknown) => string }).key(undefined)).toBe(
          "environment-catalog",
        );
      }

      const service = {
        register: vi.fn((input: unknown) => Effect.succeed(input)),
        connect: vi.fn((input: unknown) => Effect.succeed(input)),
        disconnect: vi.fn((input: unknown) => Effect.succeed(input)),
        remove: vi.fn((input: unknown) => Effect.succeed(input)),
        removeRelayEnvironments: vi.fn(() => Effect.void),
        retryNow: vi.fn((input: unknown) => Effect.succeed(input)),
        acceptStorageIdentity: vi.fn((input: unknown) => Effect.succeed(input)),
        rename: vi.fn((id: unknown, label: unknown) => Effect.succeed({ id, label })),
      };
      const inputs = [
        { id: "target" },
        environmentId,
        environmentId,
        environmentId,
        undefined,
        environmentId,
        environmentId,
        { environmentId, label: "GPU box" },
      ];
      for (const [index, input] of inputs.entries()) {
        const execute = harness.commandConfigs[index]!.execute as (
          value: unknown,
        ) => Effect.Effect<unknown>;
        yield* execute(input).pipe(Effect.provideService(EnvironmentRegistry, service as never));
      }
      expect(service.register).toHaveBeenCalledWith({ id: "target" });
      expect(service.connect).toHaveBeenCalledWith(environmentId);
      expect(service.disconnect).toHaveBeenCalledWith(environmentId);
      expect(service.remove).toHaveBeenCalledWith(environmentId);
      expect(service.removeRelayEnvironments).toHaveBeenCalled();
      expect(service.retryNow).toHaveBeenCalledWith(environmentId);
      expect(service.acceptStorageIdentity).toHaveBeenCalledWith(environmentId);
      expect(service.rename).toHaveBeenCalledWith(environmentId, "GPU box");
      registry.dispose();
    }),
  );
});
