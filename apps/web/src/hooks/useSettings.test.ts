/**
 * Tests for the environment-scoped settings hooks.
 *
 * The module splits settings between server-authoritative state (atom-backed,
 * persisted over RPC) and client-only state (persisted through the local API and
 * mirrored in a module-level snapshot with a `useSyncExternalStore` bridge). We
 * mock React's store/memo/callback hooks to plain passthroughs so the hooks can
 * be invoked as functions, and stub the persistence + atom seams. The exported
 * `__reset*`/`__set*` test helpers reset the module singletons between cases.
 */
import {
  DEFAULT_SERVER_SETTINGS,
  ProviderDriverKind,
  ProviderInstanceId,
  EnvironmentId,
  type ServerConfig,
  WS_METHODS,
} from "@coddy/contracts";
import { DEFAULT_CLIENT_SETTINGS } from "@coddy/contracts/settings";
import { it as effectIt } from "@effect/vitest";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import * as Effect from "effect/Effect";
import * as Layer from "effect/Layer";
import * as Option from "effect/Option";
import * as SubscriptionRef from "effect/SubscriptionRef";
import { AsyncResult, Atom, AtomRegistry } from "effect/reactivity";
import { EnvironmentRegistry, EnvironmentSupervisor } from "@coddy/client-runtime/connection";
import { createServerEnvironmentAtoms } from "@coddy/client-runtime/state/server";

const h = vi.hoisted(() => ({
  persisted: null as Record<string, unknown> | null,
  getClientSettings: undefined as unknown as () => Promise<Record<string, unknown> | null>,
  setClientSettings: undefined as unknown as (settings: unknown) => Promise<void>,
  persistServerSettings: undefined as unknown as (input: unknown) => Promise<unknown>,
  primaryEnv: null as { environmentId: EnvironmentId } | null,
  serverSettingsValue: null as unknown,
  primaryServerSettings: null as unknown,
  realAtomRegistry: null as unknown,
  realPrimaryServerSettingsAtom: null as unknown,
  realServerUpdateSettingsCommand: null as unknown,
  unsubs: [] as Array<() => void>,
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useCallback: (<T>(fn: T) => fn) as typeof actual.useCallback,
    useMemo: (<T>(fn: () => T) => fn()) as typeof actual.useMemo,
    useSyncExternalStore: ((
      subscribe: (l: () => void) => () => void,
      getSnapshot: () => unknown,
    ) => {
      const unsubscribe = subscribe(() => {});
      h.unsubs.push(unsubscribe);
      return getSnapshot();
    }) as typeof actual.useSyncExternalStore,
  };
});

vi.mock("@effect/atom-react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@effect/atom-react")>();
  return {
    ...actual,
    useAtomValue: (atom: { __atom?: string } | null) => {
      if (atom && atom.__atom === "primaryServerSettings") {
        if (h.realAtomRegistry !== null && h.realPrimaryServerSettingsAtom !== null) {
          return (
            h.realAtomRegistry as {
              get: (target: unknown) => unknown;
            }
          ).get(h.realPrimaryServerSettingsAtom);
        }
        return h.primaryServerSettings;
      }
      return h.serverSettingsValue;
    },
  };
});

vi.mock("~/localApi", () => ({
  ensureLocalApi: () => ({
    persistence: {
      getClientSettings: () => h.getClientSettings(),
      setClientSettings: (settings: unknown) => h.setClientSettings(settings),
    },
  }),
}));

vi.mock("~/state/server", () => ({
  primaryServerSettingsAtom: { __atom: "primaryServerSettings" },
  serverEnvironment: {
    settingsValueAtom: (environmentId: unknown) => ({ __atom: "settingsValue", environmentId }),
    updateSettings: { __command: "updateSettings" },
  },
}));

vi.mock("~/state/environments", () => ({
  usePrimaryEnvironment: () => h.primaryEnv,
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => (input: unknown) => {
    if (h.realAtomRegistry !== null && h.realServerUpdateSettingsCommand !== null) {
      return (
        h.realServerUpdateSettingsCommand as {
          run: (registry: unknown, value: unknown) => Promise<unknown>;
        }
      ).run(h.realAtomRegistry, input);
    }
    return h.persistServerSettings(input);
  },
}));

vi.mock("@coddy/client-runtime/errors", () => ({
  safeErrorLogAttributes: () => ({}),
}));

import {
  __resetClientSettingsPersistenceForTests,
  __setClientSettingsForTests,
  getClientSettings,
  mergeEnvironmentSettings,
  useClientSettings,
  useClientSettingsHydrated,
  useEnvironmentSettings,
  usePrimarySettings,
  useUpdateClientSettings,
  useUpdateEnvironmentSettings,
  useUpdatePrimarySettings,
} from "./useSettings";

const environmentId = EnvironmentId.make("environment-1");

const installRejectedSettingsCommand = Effect.fn("installRejectedSettingsCommand")(function* (
  initialSettings = DEFAULT_SERVER_SETTINGS,
) {
  const calls: Array<unknown> = [];
  const session = yield* SubscriptionRef.make(
    Option.some({
      client: {
        [WS_METHODS.serverUpdateSettings]: (input: unknown) =>
          Effect.gen(function* () {
            calls.push(input);
            return yield* Effect.fail({
              _tag: "ServerSettingsError",
              message: "settings update rejected",
            });
          }),
      },
    } as never),
  );
  const supervisor = EnvironmentSupervisor.of({
    target: { environmentId, label: "Settings environment" },
    session,
  } as never);
  const selectedEnvironments: Array<EnvironmentId> = [];
  const environmentRegistry = EnvironmentRegistry.of({
    run: (selectedEnvironmentId, effect) => {
      selectedEnvironments.push(selectedEnvironmentId);
      return Effect.provideService(effect, EnvironmentSupervisor, supervisor);
    },
  } as EnvironmentRegistry["Service"]);
  const initialConfig = {
    availableEditors: [],
    issues: [],
    keybindings: {},
    keybindingsConfigPath: null,
    observability: null,
    providers: [],
    settings: initialSettings,
  } as unknown as ServerConfig;
  const serverAtoms = createServerEnvironmentAtoms(
    Atom.runtime(Layer.succeed(EnvironmentRegistry, environmentRegistry)),
    {
      initialConfigValueAtom: () => Atom.make(initialConfig),
    },
  );
  const atomRegistry = AtomRegistry.make();
  h.realAtomRegistry = atomRegistry;
  h.realPrimaryServerSettingsAtom = serverAtoms.settingsValueAtom(environmentId);
  h.realServerUpdateSettingsCommand = serverAtoms.updateSettings;

  return { atomRegistry, calls, selectedEnvironments };
});

async function flush(): Promise<void> {
  for (let index = 0; index < 6; index += 1) {
    await Promise.resolve();
  }
}

beforeEach(() => {
  __resetClientSettingsPersistenceForTests();
  h.persisted = null;
  h.getClientSettings = vi.fn(() => Promise.resolve(h.persisted));
  h.setClientSettings = vi.fn(() => Promise.resolve());
  h.persistServerSettings = vi.fn(() => Promise.resolve({ ok: true }));
  h.primaryEnv = null;
  h.serverSettingsValue = null;
  h.primaryServerSettings = DEFAULT_SERVER_SETTINGS;
  h.realAtomRegistry = null;
  h.realPrimaryServerSettingsAtom = null;
  h.realServerUpdateSettingsCommand = null;
  h.unsubs.length = 0;
});

afterEach(() => {
  __resetClientSettingsPersistenceForTests();
});

describe("mergeEnvironmentSettings", () => {
  it("combines the selected environment's server settings with client preferences", () => {
    const serverSettings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: {
        [ProviderInstanceId.make("codex_remote")]: {
          driver: ProviderDriverKind.make("codex"),
          enabled: true,
        },
      },
    };
    const clientSettings = {
      ...DEFAULT_CLIENT_SETTINGS,
      favorites: [
        {
          provider: ProviderInstanceId.make("codex_remote"),
          model: "gpt-5.4",
        },
      ],
    };

    const settings = mergeEnvironmentSettings(serverSettings, clientSettings);

    expect(settings.providerInstances).toBe(serverSettings.providerInstances);
    expect(settings.favorites).toBe(clientSettings.favorites);
  });
});

describe("client settings hydration", () => {
  it("starts unhydrated with the default snapshot", () => {
    expect(getClientSettings()).toBe(DEFAULT_CLIENT_SETTINGS);
    expect(useClientSettingsHydrated()).toBe(false);
  });

  it("hydrates the snapshot from persisted client settings and flips the hydrated flag", async () => {
    h.persisted = { wordWrap: false };
    // Subscribing (via useSyncExternalStore) kicks off hydration.
    useClientSettings();
    await flush();

    expect(h.getClientSettings).toHaveBeenCalledTimes(1);
    expect(getClientSettings()).toMatchObject({ wordWrap: false });
    expect(useClientSettingsHydrated()).toBe(true);
  });

  it("schema-recovers a malformed native terminal font without losing valid preferences", async () => {
    h.persisted = {
      wordWrap: false,
      terminalFontPreference: { mode: "removed-preset" },
    };

    useClientSettings();
    await flush();

    expect(getClientSettings()).toMatchObject({
      wordWrap: false,
      terminalFontPreference: { mode: "bundled" },
    });
  });

  it("keeps defaults but still hydrates when persistence returns nothing", async () => {
    h.persisted = null;
    useClientSettingsHydrated();
    await flush();

    expect(getClientSettings()).toBe(DEFAULT_CLIENT_SETTINGS);
    expect(useClientSettingsHydrated()).toBe(true);
  });

  it("logs and still hydrates when reading persisted settings throws", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    h.getClientSettings = vi.fn(() => Promise.reject(new Error("storage unavailable")));

    useClientSettings();
    await flush();

    expect(errorSpy).toHaveBeenCalled();
    expect(String(errorSpy.mock.calls[0]![0])).toContain("[CLIENT_SETTINGS]");
    expect(useClientSettingsHydrated()).toBe(true);
    errorSpy.mockRestore();
  });

  it("only hydrates once even across many subscribers", async () => {
    h.persisted = { wordWrap: false };
    useClientSettings();
    useClientSettingsHydrated();
    useClientSettings();
    await flush();
    expect(h.getClientSettings).toHaveBeenCalledTimes(1);
  });

  it("unsubscribing removes the listener without throwing", async () => {
    useClientSettings();
    await flush();
    for (const unsubscribe of h.unsubs) {
      expect(() => unsubscribe()).not.toThrow();
    }
  });
});

describe("__setClientSettingsForTests", () => {
  it("seeds a hydrated snapshot directly", () => {
    const seeded = { ...DEFAULT_CLIENT_SETTINGS, wordWrap: false };
    __setClientSettingsForTests(seeded);
    expect(getClientSettings()).toBe(seeded);
    expect(useClientSettingsHydrated()).toBe(true);
  });
});

describe("useClientSettings selector", () => {
  it("returns the whole snapshot or a projected slice", () => {
    __setClientSettingsForTests({ ...DEFAULT_CLIENT_SETTINGS, wordWrap: false });
    expect(useClientSettings()).toMatchObject({ wordWrap: false });
    expect(useClientSettings((settings) => settings.wordWrap)).toBe(false);
  });
});

describe("useEnvironmentSettings / usePrimarySettings", () => {
  it("merges the environment's server settings with client preferences", () => {
    __setClientSettingsForTests({ ...DEFAULT_CLIENT_SETTINGS, wordWrap: false });
    h.serverSettingsValue = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: { probe: { driver: "codex", enabled: true } },
    };

    const merged = useEnvironmentSettings(environmentId);
    expect(merged.providerInstances).toEqual({ probe: { driver: "codex", enabled: true } });
    expect(merged.wordWrap).toBe(false);

    // A selector projects a slice of the merged view.
    expect(useEnvironmentSettings(environmentId, (settings) => settings.wordWrap)).toBe(false);
  });

  it("falls back to default server settings when the atom has no value", () => {
    h.serverSettingsValue = null;
    const merged = useEnvironmentSettings(environmentId);
    expect(merged.providerInstances).toEqual(DEFAULT_SERVER_SETTINGS.providerInstances);
  });

  it("reads primary-only server settings for global surfaces", () => {
    h.primaryServerSettings = {
      ...DEFAULT_SERVER_SETTINGS,
      providerInstances: { primary: { driver: "codex", enabled: false } },
    };
    const merged = usePrimarySettings();
    expect(merged.providerInstances).toEqual({ primary: { driver: "codex", enabled: false } });
  });
});

describe("useUpdateClientSettings", () => {
  it("persists a client-only patch and updates the live snapshot", () => {
    const update = useUpdateClientSettings();
    update({ wordWrap: false });

    expect(getClientSettings()).toMatchObject({ wordWrap: false });
    expect(h.setClientSettings).toHaveBeenCalledTimes(1);
    expect(h.setClientSettings).toHaveBeenCalledWith(expect.objectContaining({ wordWrap: false }));
  });

  it("logs a persistence failure without throwing", async () => {
    const errorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    h.setClientSettings = vi.fn(() => Promise.reject(new Error("quota exceeded")));
    useUpdateClientSettings()({ wordWrap: false });
    await flush();
    expect(errorSpy).toHaveBeenCalled();
    expect(String(errorSpy.mock.calls[0]![0])).toContain("persist failed");
    errorSpy.mockRestore();
  });
});

describe("useUpdateEnvironmentSettings routing", () => {
  it("routes server keys to the RPC command and client keys to local persistence", () => {
    const update = useUpdateEnvironmentSettings(environmentId);
    update({
      providerInstances: { probe: { driver: ProviderDriverKind.make("codex"), enabled: true } },
      wordWrap: false,
    } as never);

    expect(h.persistServerSettings).toHaveBeenCalledTimes(1);
    expect(h.persistServerSettings).toHaveBeenCalledWith({
      environmentId,
      input: {
        patch: {
          providerInstances: {
            probe: { driver: ProviderDriverKind.make("codex"), enabled: true },
          },
        },
      },
    });
    expect(h.setClientSettings).toHaveBeenCalledTimes(1);
    expect(getClientSettings()).toMatchObject({ wordWrap: false });
  });

  it("does not persist a client patch when only server keys change", () => {
    useUpdateEnvironmentSettings(environmentId)({
      providerInstances: {},
    } as never);
    expect(h.persistServerSettings).toHaveBeenCalledTimes(1);
    expect(h.setClientSettings).not.toHaveBeenCalled();
  });
});

describe("useUpdatePrimarySettings", () => {
  effectIt.effect(
    "rolls back a rejected terminal activity update without changing chat activity",
    () =>
      Effect.gen(function* () {
        h.primaryEnv = { environmentId };
        const { atomRegistry, calls, selectedEnvironments } = yield* installRejectedSettingsCommand(
          {
            ...DEFAULT_SERVER_SETTINGS,
            enableChatAgentActivity: false,
          },
        );

        return yield* Effect.gen(function* () {
          const result = yield* Effect.promise<AsyncResult.AsyncResult<unknown, unknown>>(
            () =>
              useUpdatePrimarySettings()({
                enableTerminalAgentActivity: true,
              }) as Promise<AsyncResult.AsyncResult<unknown, unknown>>,
          );

          expect(AsyncResult.isFailure(result)).toBe(true);
          expect(selectedEnvironments).toEqual([environmentId]);
          expect(calls).toEqual([{ patch: { enableTerminalAgentActivity: true } }]);
          expect(usePrimarySettings()).toMatchObject({
            enableChatAgentActivity: false,
            enableTerminalAgentActivity: DEFAULT_SERVER_SETTINGS.enableTerminalAgentActivity,
          });
        }).pipe(Effect.ensuring(Effect.sync(() => atomRegistry.dispose())));
      }),
  );

  it("returns the settled server command result", async () => {
    h.primaryEnv = { environmentId };
    const settled = AsyncResult.success({ ...DEFAULT_SERVER_SETTINGS });
    h.persistServerSettings = vi.fn(() => Promise.resolve(settled));

    const result = useUpdatePrimarySettings()({ providerInstances: {} } as never);

    expect(result).toBeInstanceOf(Promise);
    await expect(result).resolves.toBe(settled);
  });

  it("targets the primary environment when one is active", () => {
    h.primaryEnv = { environmentId };
    useUpdatePrimarySettings()({ providerInstances: {} } as never);
    expect(h.persistServerSettings).toHaveBeenCalledTimes(1);
    expect(h.persistServerSettings).toHaveBeenCalledWith({
      environmentId,
      input: { patch: { providerInstances: {} } },
    });
  });

  it("drops server patches when there is no primary environment", () => {
    h.primaryEnv = null;
    useUpdatePrimarySettings()({ providerInstances: {} } as never);
    expect(h.persistServerSettings).not.toHaveBeenCalled();
  });

  it("still persists client keys with no primary environment", () => {
    h.primaryEnv = null;
    useUpdatePrimarySettings()({ wordWrap: false } as never);
    expect(h.persistServerSettings).not.toHaveBeenCalled();
    expect(h.setClientSettings).toHaveBeenCalledTimes(1);
    expect(getClientSettings()).toMatchObject({ wordWrap: false });
  });

  it("persists terminal font preferences locally without a server RPC", () => {
    h.primaryEnv = { environmentId };

    useUpdatePrimarySettings()({
      terminalFontPreference: { mode: "system" },
    } as never);

    expect(h.persistServerSettings).not.toHaveBeenCalled();
    expect(h.setClientSettings).toHaveBeenCalledTimes(1);
    expect(getClientSettings()).toMatchObject({
      terminalFontPreference: { mode: "system" },
    });
  });
});
