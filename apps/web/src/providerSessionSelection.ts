import {
  ProviderDriverKind,
  type ModelSelection,
  type ProviderInstanceId,
  type ServerProvider,
  type ServerSettings,
} from "@coddy/contracts";
import {
  resolveProviderSessionDefault,
  type ResolvedProviderSessionDefault,
} from "@coddy/shared/providerSessionDefaults";

type ProviderSessionSelectionSettings = Pick<
  ServerSettings,
  "providerInstances" | "providerSessionDefaults"
>;

/**
 * Resolve model/options after the caller has chosen a provider instance.
 *
 * Provider routing intentionally remains at each creation boundary; this
 * helper only centralizes the driver lookup and shared-default resolution.
 */
export function resolveProviderSessionSelectionForInstance(input: {
  readonly instanceId: ProviderInstanceId;
  readonly providers: ReadonlyArray<ServerProvider>;
  readonly settings: ProviderSessionSelectionSettings;
  readonly projectSelection?: ModelSelection | null;
  readonly explicitSelection?: ModelSelection | null;
}): ResolvedProviderSessionDefault {
  const provider = input.providers.find((candidate) => candidate.instanceId === input.instanceId);
  const driver =
    provider?.driver ??
    input.settings.providerInstances[input.instanceId]?.driver ??
    ProviderDriverKind.make(input.instanceId);
  const configuredDefault = input.settings.providerSessionDefaults[driver];
  return resolveProviderSessionDefault({
    driver,
    instanceId: input.instanceId,
    models: provider?.models ?? [],
    ...(configuredDefault === undefined ? {} : { configuredDefault }),
    ...(input.projectSelection === undefined ? {} : { projectSelection: input.projectSelection }),
    ...(input.explicitSelection === undefined
      ? {}
      : { explicitSelection: input.explicitSelection }),
  });
}
