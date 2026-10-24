import {
  type EnvironmentId,
  type ServerConfig,
  type ServerProviderCapabilitiesInput,
  type ServerConfigStreamEvent,
  type ServerLifecycleWelcomePayload,
  WS_METHODS,
} from "@bibcode/contracts";
import * as Equal from "effect/Equal";
import * as Option from "effect/Option";
import * as Stream from "effect/Stream";
import { AsyncResult, Atom } from "effect/reactivity";

import {
  createAtomCommandScheduler,
  createEnvironmentRpcCommand,
  createEnvironmentQueryAtomFamily,
  createEnvironmentRpcQueryAtomFamily,
  createEnvironmentRpcSubscriptionAtomFamily,
} from "./runtime.ts";
import { request } from "../rpc/client.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";
import { applyServerConfigEvent } from "../rpc/sharedServerConfig.ts";

export const PROVIDER_CAPABILITIES_STALE_TIME_MS = 30_000;

export interface ServerConfigProjection {
  readonly config: ServerConfig;
  readonly latestEvent: ServerConfigStreamEvent;
  readonly capabilitiesRevision: number;
}

export function applyServerConfigProjection(
  current: Option.Option<ServerConfigProjection>,
  event: ServerConfigStreamEvent,
): Option.Option<ServerConfigProjection> {
  const previous = Option.getOrNull(current);
  // Every reconnect re-sends the whole config (about 70 KB). Keeping the projection when nothing
  // changed spares every reader a new copy and the re-render it would cause.
  if (
    previous !== null &&
    event.type === "snapshot" &&
    Equal.equals(previous.config, event.config)
  ) {
    return current;
  }
  const config = applyServerConfigEvent(previous?.config ?? null, event);
  const previousRevision = previous?.capabilitiesRevision ?? 0;
  const capabilitiesRevision =
    previousRevision + (capabilitiesMayHaveChanged(previous?.config ?? null, event) ? 1 : 0);
  return config === null
    ? Option.none()
    : Option.some({ config, latestEvent: event, capabilitiesRevision });
}

/**
 * Whether `event` can change provider capabilities. Each new revision is a new capabilities key,
 * so a bump costs every composer a `server.getProviderCapabilities` read (#96).
 *
 * An update always counts: settings arrive with secrets redacted, so equal payloads can still
 * hide a changed secret. Every reconnect re-sends the snapshot, and the new connection already
 * re-reads the mounted capabilities query, so a snapshot counts only when its providers or
 * settings differ from the config already projected.
 */
function capabilitiesMayHaveChanged(
  previous: ServerConfig | null,
  event: ServerConfigStreamEvent,
): boolean {
  switch (event.type) {
    case "keybindingsUpdated":
      return false;
    case "snapshot":
      return (
        previous === null ||
        !Equal.equals(previous.providers, event.config.providers) ||
        !Equal.equals(previous.settings, event.config.settings)
      );
    case "providerStatuses":
    case "settingsUpdated":
      return true;
  }
}

export function projectServerConfig(
  current: Option.Option<ServerConfigProjection>,
  event: ServerConfigStreamEvent,
): readonly [Option.Option<ServerConfigProjection>, ReadonlyArray<ServerConfigProjection>] {
  const next = applyServerConfigProjection(current, event);
  return [next, Option.toArray(next)];
}

export function projectServerWelcome(
  current: Option.Option<ServerLifecycleWelcomePayload>,
  event: {
    readonly type: "welcome" | "ready";
    readonly payload: unknown;
  },
): readonly [
  Option.Option<ServerLifecycleWelcomePayload>,
  ReadonlyArray<ServerLifecycleWelcomePayload>,
] {
  if (event.type !== "welcome") {
    return [current, []];
  }
  const welcome = event.payload as ServerLifecycleWelcomePayload;
  return [Option.some(welcome), [welcome]];
}

export function createServerEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | R, E>,
  options: {
    readonly initialConfigValueAtom: (
      environmentId: EnvironmentId,
    ) => Atom.Atom<ServerConfig | null>;
  },
) {
  const configScheduler = createAtomCommandScheduler();
  const configConcurrency = {
    mode: "serial" as const,
    key: ({ environmentId }: { readonly environmentId: string }) => environmentId,
  };
  const configProjection = createEnvironmentRpcSubscriptionAtomFamily(runtime, {
    label: "environment-data:server:config-projection",
    tag: WS_METHODS.subscribeServerConfig,
    transform: (stream) =>
      stream.pipe(Stream.mapAccum(Option.none<ServerConfigProjection>, projectServerConfig)),
  });
  const emptyConfigAtom = Atom.make<ServerConfig | null>(null).pipe(
    Atom.withLabel("environment-data:server:config:empty"),
  );
  const configValueAtom = Atom.family((environmentId: EnvironmentId | null) => {
    if (environmentId === null) {
      return emptyConfigAtom;
    }
    return Atom.make((get): ServerConfig | null => {
      const projection = Option.getOrNull(
        AsyncResult.value(get(configProjection({ environmentId, input: {} }))),
      );
      return projection?.config ?? get(options.initialConfigValueAtom(environmentId));
    }).pipe(Atom.withLabel(`environment-data:server:config:${environmentId}`));
  });
  const settingsValueAtom = Atom.family((environmentId: EnvironmentId) =>
    Atom.make((get) => get(configValueAtom(environmentId))?.settings ?? null).pipe(
      Atom.withLabel(`environment-data:server:settings:${environmentId}`),
    ),
  );
  const providersValueAtom = Atom.family((environmentId: EnvironmentId) =>
    Atom.make((get) => get(configValueAtom(environmentId))?.providers ?? null).pipe(
      Atom.withLabel(`environment-data:server:providers:${environmentId}`),
    ),
  );

  return {
    configValueAtom,
    capabilitiesRevisionAtom: Atom.family((environmentId: EnvironmentId | null) =>
      Atom.make((get) =>
        environmentId === null
          ? 0
          : (Option.getOrNull(
              AsyncResult.value(get(configProjection({ environmentId, input: {} }))),
            )?.capabilitiesRevision ?? 0),
      ),
    ),
    providerCapabilities: createEnvironmentQueryAtomFamily(runtime, {
      label: "environment-data:server:provider-capabilities",
      staleTimeMs: PROVIDER_CAPABILITIES_STALE_TIME_MS,
      transportCutoffKey: ({
        instanceId,
        cwd,
      }: ServerProviderCapabilitiesInput & { readonly revision: number }) => ({ instanceId, cwd }),
      execute: ({
        instanceId,
        cwd,
      }: ServerProviderCapabilitiesInput & { readonly revision: number }) =>
        request(WS_METHODS.serverGetProviderCapabilities, { instanceId, cwd }),
    }),
    settingsValueAtom,
    providersValueAtom,
    traceDiagnostics: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:server:trace-diagnostics",
      tag: WS_METHODS.serverGetTraceDiagnostics,
    }),
    processDiagnostics: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:server:process-diagnostics",
      tag: WS_METHODS.serverGetProcessDiagnostics,
    }),
    profilerStatus: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:server:profiler-status",
      tag: WS_METHODS.serverGetProfilerStatus,
    }),
    reportProfilerClientBatch: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:report-profiler-client-batch",
      tag: WS_METHODS.serverReportProfilerClientBatch,
    }),
    processResourceHistory: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:server:process-resource-history",
      tag: WS_METHODS.serverGetProcessResourceHistory,
    }),
    providerUsage: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:server:provider-usage",
      tag: WS_METHODS.serverGetProviderUsage,
    }),
    configProjection,
    welcome: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:server:welcome",
      tag: WS_METHODS.subscribeServerLifecycle,
      transform: (stream) =>
        stream.pipe(
          Stream.mapAccum(Option.none<ServerLifecycleWelcomePayload>, projectServerWelcome),
        ),
    }),
    refreshProviders: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:refresh-providers",
      tag: WS_METHODS.serverRefreshProviders,
      concurrency: {
        mode: "singleFlight",
        key: ({ environmentId }) => environmentId,
      },
    }),
    updateProvider: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:update-provider",
      tag: WS_METHODS.serverUpdateProvider,
      scheduler: configScheduler,
      concurrency: configConcurrency,
    }),
    upsertKeybinding: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:upsert-keybinding",
      tag: WS_METHODS.serverUpsertKeybinding,
      scheduler: configScheduler,
      concurrency: configConcurrency,
    }),
    removeKeybinding: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:remove-keybinding",
      tag: WS_METHODS.serverRemoveKeybinding,
      scheduler: configScheduler,
      concurrency: configConcurrency,
    }),
    updateSettings: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:update-settings",
      tag: WS_METHODS.serverUpdateSettings,
      scheduler: configScheduler,
      concurrency: configConcurrency,
    }),
    signalProcess: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:signal-process",
      tag: WS_METHODS.serverSignalProcess,
    }),
    refreshProviderUsage: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:refresh-provider-usage",
      tag: WS_METHODS.serverRefreshProviderUsage,
    }),
    consumeCodexRateLimitReset: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:server:consume-codex-rate-limit-reset",
      tag: WS_METHODS.serverConsumeCodexRateLimitReset,
      concurrency: {
        mode: "singleFlight",
        key: ({ environmentId }) => environmentId,
      },
    }),
  };
}
