import {
  defaultInstanceIdForDriver,
  isProviderDriverKind,
  type ModelSelection,
  ProviderDriverKind,
  type ProviderInstanceId,
  type ServerProvider,
} from "@coddy/contracts";
import { threadHasStarted } from "./components/ChatView.logic";
import type { Thread } from "./types";

export interface ThreadProviderBinding {
  readonly instanceId: ProviderInstanceId;
  readonly driver: ProviderDriverKind | null;
  readonly status: ServerProvider | null;
  readonly lockedProvider: ProviderDriverKind | null;
  readonly lockedProviderInstanceId: ProviderInstanceId | null;
  readonly conflict: ThreadProviderBindingConflict | null;
}

export interface ThreadProviderBindingConflict {
  readonly kind: "session-provider-driver-mismatch";
  readonly instanceId: ProviderInstanceId;
  readonly expectedDriver: ProviderDriverKind;
  readonly observedDriver: ProviderDriverKind;
  readonly reason: string;
}

interface ResolveThreadProviderBindingInput {
  readonly thread: Thread | null | undefined;
  readonly projectDefaultModelSelection: ModelSelection | null | undefined;
  readonly selectedProviderInstanceId: ProviderInstanceId | null;
  readonly providers: ReadonlyArray<ServerProvider>;
}

function findProvider(
  providers: ReadonlyArray<ServerProvider>,
  instanceId: ProviderInstanceId | null | undefined,
): ServerProvider | null {
  if (!instanceId) return null;
  return providers.find((provider) => provider.instanceId === instanceId) ?? null;
}

function resolveDriverFallback(
  driver: ProviderDriverKind,
  providers: ReadonlyArray<ServerProvider>,
  candidates: ReadonlyArray<ProviderInstanceId | null | undefined>,
  rejectedInstanceId: ProviderInstanceId | null,
): Pick<ThreadProviderBinding, "instanceId" | "status"> {
  for (const candidate of candidates) {
    if (!candidate || candidate === rejectedInstanceId) continue;
    const status = findProvider(providers, candidate);
    if (status?.enabled && status.driver === driver) {
      return { instanceId: status.instanceId, status };
    }
  }

  const defaultInstanceId = defaultInstanceIdForDriver(driver);
  const defaultStatus = findProvider(providers, defaultInstanceId);
  if (defaultStatus?.enabled && defaultStatus.driver === driver) {
    return { instanceId: defaultStatus.instanceId, status: defaultStatus };
  }

  const matchingStatus =
    providers.find(
      (provider) =>
        provider.enabled &&
        provider.driver === driver &&
        provider.instanceId !== rejectedInstanceId,
    ) ?? null;
  return {
    instanceId: matchingStatus?.instanceId ?? defaultInstanceId,
    status: matchingStatus,
  };
}

export function resolveThreadProviderBinding(
  input: ResolveThreadProviderBindingInput,
): ThreadProviderBinding {
  const threadModelInstanceId = input.thread?.modelSelection.instanceId ?? null;
  const projectDefaultInstanceId = input.projectDefaultModelSelection?.instanceId ?? null;

  if (!threadHasStarted(input.thread)) {
    const candidates = [
      input.selectedProviderInstanceId,
      threadModelInstanceId,
      projectDefaultInstanceId,
    ];
    for (const candidate of candidates) {
      const status = findProvider(input.providers, candidate);
      if (status?.enabled) {
        return {
          instanceId: status.instanceId,
          driver: status.driver,
          status,
          lockedProvider: null,
          lockedProviderInstanceId: null,
          conflict: null,
        };
      }
    }
    const fallbackStatus = input.providers.find((provider) => provider.enabled) ?? null;
    const fallbackInstanceId =
      fallbackStatus?.instanceId ??
      input.selectedProviderInstanceId ??
      threadModelInstanceId ??
      projectDefaultInstanceId ??
      defaultInstanceIdForDriver(ProviderDriverKind.make("codex"));
    return {
      instanceId: fallbackInstanceId,
      driver: fallbackStatus?.driver ?? null,
      status: fallbackStatus,
      lockedProvider: null,
      lockedProviderInstanceId: null,
      conflict: null,
    };
  }

  const sessionInstanceId = input.thread?.session?.providerInstanceId ?? null;
  const sessionDriverCandidate = input.thread?.session?.providerName ?? null;
  const sessionDriver = isProviderDriverKind(sessionDriverCandidate)
    ? sessionDriverCandidate
    : null;

  if (sessionInstanceId) {
    const sessionStatus = findProvider(input.providers, sessionInstanceId);
    if (sessionStatus && sessionDriver && sessionStatus.driver !== sessionDriver) {
      return {
        instanceId: sessionInstanceId,
        driver: null,
        status: null,
        lockedProvider: null,
        lockedProviderInstanceId: sessionInstanceId,
        conflict: {
          kind: "session-provider-driver-mismatch",
          instanceId: sessionInstanceId,
          expectedDriver: sessionDriver,
          observedDriver: sessionStatus.driver,
          reason: `Provider instance "${sessionInstanceId}" reports driver "${sessionStatus.driver}", but the active session expects "${sessionDriver}". Sending is blocked until provider metadata agrees.`,
        },
      };
    }
    if (sessionStatus) {
      const driver = sessionDriver ?? sessionStatus.driver;
      return {
        instanceId: sessionInstanceId,
        driver,
        status: sessionStatus,
        lockedProvider: driver,
        lockedProviderInstanceId: null,
        conflict: null,
      };
    }
    if (!sessionDriver) {
      return {
        instanceId: sessionInstanceId,
        driver: null,
        status: null,
        lockedProvider: null,
        lockedProviderInstanceId: sessionInstanceId,
        conflict: null,
      };
    }
    return {
      instanceId: sessionInstanceId,
      driver: sessionDriver,
      status: null,
      lockedProvider: sessionDriver,
      lockedProviderInstanceId: sessionInstanceId,
      conflict: null,
    };
  }

  if (sessionDriver) {
    const fallback = resolveDriverFallback(
      sessionDriver,
      input.providers,
      [threadModelInstanceId, projectDefaultInstanceId],
      sessionInstanceId,
    );
    return {
      ...fallback,
      driver: sessionDriver,
      lockedProvider: sessionDriver,
      lockedProviderInstanceId: null,
      conflict: null,
    };
  }

  const persistedInstanceId = threadModelInstanceId ?? projectDefaultInstanceId;
  const persistedStatus = findProvider(input.providers, persistedInstanceId);
  if (persistedInstanceId && persistedStatus) {
    return {
      instanceId: persistedInstanceId,
      driver: persistedStatus.driver,
      status: persistedStatus,
      lockedProvider: persistedStatus.driver,
      lockedProviderInstanceId: null,
      conflict: null,
    };
  }
  if (persistedInstanceId) {
    return {
      instanceId: persistedInstanceId,
      driver: null,
      status: null,
      lockedProvider: null,
      lockedProviderInstanceId: persistedInstanceId,
      conflict: null,
    };
  }

  const fallbackStatus = input.providers.find((provider) => provider.enabled) ?? null;
  const fallbackInstanceId =
    fallbackStatus?.instanceId ?? defaultInstanceIdForDriver(ProviderDriverKind.make("codex"));
  return {
    instanceId: fallbackInstanceId,
    driver: fallbackStatus?.driver ?? null,
    status: fallbackStatus,
    lockedProvider: fallbackStatus?.driver ?? null,
    lockedProviderInstanceId: fallbackStatus ? null : fallbackInstanceId,
    conflict: null,
  };
}
