import {
  DEFAULT_MODEL,
  DEFAULT_MODEL_BY_PROVIDER,
  type ModelSelection,
  ProviderDriverKind,
  type ProviderInstanceId,
  type ProviderOptionDescriptor,
  type ProviderOptionSelection,
  type ProviderSessionDefault,
  type SelectProviderOptionDescriptor,
  type ServerProviderModel,
} from "@coddy/contracts";

import {
  buildProviderOptionSelectionsFromDescriptors,
  createModelSelection,
  getProviderCapabilityDescriptors,
  getProviderOptionCurrentValue,
  PROVIDER_EFFORT_OPTION_IDS,
  resolveSelectableModel,
} from "./model.ts";

export const PROVIDER_SESSION_EFFORT_OPTION_IDS = PROVIDER_EFFORT_OPTION_IDS;

export type ProviderSessionDefaultFallbackReason =
  | "configured-model-unavailable"
  | "models-unavailable";

export interface ProviderSessionDefaultFallback {
  readonly driver: ProviderDriverKind;
  readonly instanceId: ProviderInstanceId;
  readonly configuredModel: string;
  readonly resolvedModel: string;
  readonly reason: ProviderSessionDefaultFallbackReason;
}

export interface ResolvedProviderSessionDefault {
  readonly modelSelection: ModelSelection;
  readonly effort: string | null;
  readonly fastMode: boolean | null;
  readonly configuredModelAvailable: boolean;
  readonly fallback: ProviderSessionDefaultFallback | null;
}

export interface ProviderSessionDefaultControls {
  readonly configuredModel: string;
  readonly resolvedModel: string;
  readonly modelAvailable: boolean;
  readonly effortDescriptor: SelectProviderOptionDescriptor | null;
  readonly effort: string | null;
  readonly fastModeSupported: boolean;
  readonly fastMode: boolean | null;
}

export type ProviderSessionDefaultChange =
  | { readonly type: "model"; readonly value: string }
  | { readonly type: "effort"; readonly value: string }
  | { readonly type: "fastMode"; readonly value: boolean };

interface NormalizedProviderOptions {
  readonly descriptors: ReadonlyArray<ProviderOptionDescriptor>;
  readonly effortDescriptor: SelectProviderOptionDescriptor | null;
  readonly effort: string | null;
  readonly fastModeSupported: boolean;
  readonly fastMode: boolean | null;
}

const FAST_MODE_OPTION_ID = "fastMode";
const SERVICE_TIER_OPTION_ID = "serviceTier";
const FAST_SERVICE_TIER_VALUES = ["priority", "fast"] as const;
const CODEX_DRIVER_KIND = ProviderDriverKind.make("codex");
const CLAUDE_DRIVER_KIND = ProviderDriverKind.make("claudeAgent");
const DEFAULT_EFFORT_BY_PROVIDER: Partial<Record<ProviderDriverKind, string>> = {
  [CODEX_DRIVER_KIND]: "medium",
  [CLAUDE_DRIVER_KIND]: "high",
};

function selectedString(
  selections: ReadonlyArray<ProviderOptionSelection> | undefined,
  ids: ReadonlyArray<string>,
): string | null {
  const selection = selections?.find(
    (candidate) => ids.includes(candidate.id) && typeof candidate.value === "string",
  );
  return typeof selection?.value === "string" ? selection.value : null;
}

function getInvariantProviderModel(
  driver: ProviderDriverKind,
  model: string,
  selections: ReadonlyArray<ProviderOptionSelection> | undefined,
): ServerProviderModel | null {
  if (driver !== CODEX_DRIVER_KIND && driver !== CLAUDE_DRIVER_KIND) {
    return null;
  }

  if (driver === CODEX_DRIVER_KIND) {
    return {
      slug: model,
      name: model,
      isCustom: false,
      capabilities: null,
    };
  }

  const effortId = "effort";
  const effort =
    selectedString(selections, PROVIDER_SESSION_EFFORT_OPTION_IDS) ??
    DEFAULT_EFFORT_BY_PROVIDER[driver]!;
  const optionDescriptors: Array<ProviderOptionDescriptor> = [
    {
      id: effortId,
      label: "Reasoning",
      type: "select",
      options: [{ id: effort, label: effort, isDefault: true }],
      currentValue: effort,
    },
  ];

  return {
    slug: model,
    name: model,
    isCustom: false,
    capabilities: { optionDescriptors },
  };
}

function selectableModels(
  models: ReadonlyArray<ServerProviderModel>,
): ReadonlyArray<{ readonly slug: string; readonly name: string }> {
  return models.map(({ slug, name }) => ({ slug, name }));
}

function getFallbackModel(models: ReadonlyArray<ServerProviderModel>): ServerProviderModel | null {
  return models.find((model) => !model.isCustom) ?? models[0] ?? null;
}

function getProviderDefaultModel(driver: ProviderDriverKind): string {
  return DEFAULT_MODEL_BY_PROVIDER[driver] ?? DEFAULT_MODEL;
}

function findSelectableModel(
  driver: ProviderDriverKind,
  models: ReadonlyArray<ServerProviderModel>,
  value: string,
): ServerProviderModel | null {
  const slug = resolveSelectableModel(driver, value, selectableModels(models));
  if (!slug) {
    return null;
  }
  return models.find((model) => model.slug === slug) ?? null;
}

function findEffortDescriptor(
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
): SelectProviderOptionDescriptor | null {
  for (const id of PROVIDER_SESSION_EFFORT_OPTION_IDS) {
    const descriptor = descriptors.find(
      (candidate): candidate is SelectProviderOptionDescriptor =>
        candidate.id === id && candidate.type === "select",
    );
    if (descriptor) {
      return descriptor;
    }
  }
  return null;
}

export function getFastModeDescriptor(
  driver: ProviderDriverKind,
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
): ProviderOptionDescriptor | null {
  const booleanDescriptor = descriptors.find(
    (descriptor) => descriptor.id === FAST_MODE_OPTION_ID && descriptor.type === "boolean",
  );
  if (booleanDescriptor) {
    return booleanDescriptor;
  }
  if (driver !== CODEX_DRIVER_KIND) {
    return null;
  }

  return (
    descriptors.find(
      (descriptor) =>
        descriptor.id === SERVICE_TIER_OPTION_ID &&
        descriptor.type === "select" &&
        getFastModeOnValue(driver, descriptor) !== null &&
        getFastModeOffValue(driver, descriptor) !== null,
    ) ?? null
  );
}

export function getFastModeOnValue(
  driver: ProviderDriverKind,
  descriptor: ProviderOptionDescriptor | null | undefined,
): string | boolean | null {
  if (descriptor?.id === FAST_MODE_OPTION_ID && descriptor.type === "boolean") {
    return true;
  }
  if (
    driver !== CODEX_DRIVER_KIND ||
    descriptor?.id !== SERVICE_TIER_OPTION_ID ||
    descriptor.type !== "select"
  ) {
    return null;
  }

  return (
    FAST_SERVICE_TIER_VALUES.find((value) =>
      descriptor.options.some((option) => option.id === value),
    ) ?? null
  );
}

export function getFastModeOffValue(
  driver: ProviderDriverKind,
  descriptor: ProviderOptionDescriptor | null | undefined,
): string | boolean | null {
  if (descriptor?.id === FAST_MODE_OPTION_ID && descriptor.type === "boolean") {
    return false;
  }
  if (
    driver !== CODEX_DRIVER_KIND ||
    descriptor?.id !== SERVICE_TIER_OPTION_ID ||
    descriptor.type !== "select"
  ) {
    return null;
  }

  const nonFastOptions = descriptor.options.filter(
    (option) => !FAST_SERVICE_TIER_VALUES.some((value) => option.id === value),
  );
  const currentValue = getProviderOptionCurrentValue(descriptor);
  if (
    typeof currentValue === "string" &&
    nonFastOptions.some((option) => option.id === currentValue)
  ) {
    return currentValue;
  }
  return nonFastOptions.find((option) => option.isDefault)?.id ?? nonFastOptions[0]?.id ?? null;
}

function normalizeProviderOptions(
  driver: ProviderDriverKind,
  model: ServerProviderModel | null,
  selections?: ReadonlyArray<ProviderOptionSelection> | null,
): NormalizedProviderOptions {
  if (!model || (!model.capabilities && driver !== CODEX_DRIVER_KIND)) {
    return {
      descriptors: [],
      effortDescriptor: null,
      effort: null,
      fastModeSupported: false,
      fastMode: null,
    };
  }

  const descriptors = getProviderCapabilityDescriptors({
    provider: driver,
    caps: model.capabilities ?? {},
    selections,
    preservePromptInjectedSelections: true,
  });
  const effortDescriptor = findEffortDescriptor(descriptors);
  const effortValue = getProviderOptionCurrentValue(effortDescriptor);
  const fastModeDescriptor = getFastModeDescriptor(driver, descriptors);
  const fastModeValue = getProviderOptionCurrentValue(fastModeDescriptor);
  const fastModeOnValue = getFastModeOnValue(driver, fastModeDescriptor);

  return {
    descriptors,
    effortDescriptor,
    effort: typeof effortValue === "string" ? effortValue : null,
    fastModeSupported: fastModeDescriptor !== null,
    fastMode:
      fastModeDescriptor?.type === "boolean"
        ? typeof fastModeValue === "boolean"
          ? fastModeValue
          : null
        : fastModeDescriptor?.type === "select"
          ? fastModeValue === fastModeOnValue
          : null,
  };
}

function getPreferredSessionDefaultSource(input: {
  readonly instanceId: ProviderInstanceId;
  readonly configuredDefault?: ProviderSessionDefault | null;
  readonly projectSelection?: ModelSelection | null;
  readonly explicitSelection?: ModelSelection | null;
}): ModelSelection | ProviderSessionDefault | null {
  if (input.explicitSelection?.instanceId === input.instanceId) {
    return input.explicitSelection;
  }
  if (input.configuredDefault) {
    return input.configuredDefault;
  }
  if (input.projectSelection?.instanceId === input.instanceId) {
    return input.projectSelection;
  }
  return null;
}

export function resolveProviderSessionDefault(input: {
  readonly driver: ProviderDriverKind;
  readonly instanceId: ProviderInstanceId;
  readonly models: ReadonlyArray<ServerProviderModel>;
  readonly configuredDefault?: ProviderSessionDefault | null;
  readonly projectSelection?: ModelSelection | null;
  readonly explicitSelection?: ModelSelection | null;
}): ResolvedProviderSessionDefault {
  const preferredSource = getPreferredSessionDefaultSource(input);
  const fallbackModel = getFallbackModel(input.models);
  const configuredModel =
    preferredSource?.model ?? fallbackModel?.slug ?? getProviderDefaultModel(input.driver);
  const configuredServerModel = findSelectableModel(input.driver, input.models, configuredModel);
  const selectedModel = configuredServerModel ?? fallbackModel;
  const selections = preferredSource?.options;
  const optionsModel =
    selectedModel ?? getInvariantProviderModel(input.driver, configuredModel, selections);
  const resolvedModel =
    input.models.length === 0
      ? getProviderDefaultModel(input.driver)
      : (selectedModel?.slug ?? configuredModel);
  const normalizedOptions = normalizeProviderOptions(input.driver, optionsModel, selections);
  const fallbackReason: ProviderSessionDefaultFallbackReason | null = configuredServerModel
    ? null
    : input.models.length === 0
      ? "models-unavailable"
      : "configured-model-unavailable";

  return {
    modelSelection: createModelSelection(
      input.instanceId,
      resolvedModel,
      buildProviderOptionSelectionsFromDescriptors(normalizedOptions.descriptors),
    ),
    effort: normalizedOptions.effort,
    fastMode: normalizedOptions.fastMode,
    configuredModelAvailable: configuredServerModel !== null,
    fallback: fallbackReason
      ? {
          driver: input.driver,
          instanceId: input.instanceId,
          configuredModel,
          resolvedModel,
          reason: fallbackReason,
        }
      : null,
  };
}

export function getProviderSessionDefaultControls(input: {
  readonly driver: ProviderDriverKind;
  readonly models: ReadonlyArray<ServerProviderModel>;
  readonly configuredDefault?: ProviderSessionDefault | null;
}): ProviderSessionDefaultControls {
  const fallbackModel = getFallbackModel(input.models);
  const configuredModel =
    input.configuredDefault?.model ?? fallbackModel?.slug ?? getProviderDefaultModel(input.driver);
  const configuredServerModel = findSelectableModel(input.driver, input.models, configuredModel);
  const selectedModel = configuredServerModel ?? fallbackModel;
  const selections = input.configuredDefault?.options;
  const optionsModel =
    selectedModel ?? getInvariantProviderModel(input.driver, configuredModel, selections);
  const resolvedModel =
    input.models.length === 0 ? configuredModel : (selectedModel?.slug ?? configuredModel);
  const normalizedOptions = normalizeProviderOptions(input.driver, optionsModel, selections);

  return {
    configuredModel,
    resolvedModel,
    modelAvailable: configuredServerModel !== null,
    effortDescriptor: normalizedOptions.effortDescriptor,
    effort: normalizedOptions.effort,
    fastModeSupported: normalizedOptions.fastModeSupported,
    fastMode: normalizedOptions.fastMode,
  };
}

function replaceSelection(
  selections: ReadonlyArray<ProviderOptionSelection>,
  replacement: ProviderOptionSelection,
  replacedIds: ReadonlyArray<string> = [replacement.id],
): Array<ProviderOptionSelection> {
  return [...selections.filter((selection) => !replacedIds.includes(selection.id)), replacement];
}

function buildPersistedSelections(
  driver: ProviderDriverKind,
  normalizedOptions: NormalizedProviderOptions,
): Array<ProviderOptionSelection> | undefined {
  const selections: Array<ProviderOptionSelection> = [];
  const effortValue = getProviderOptionCurrentValue(normalizedOptions.effortDescriptor);
  if (normalizedOptions.effortDescriptor && typeof effortValue === "string") {
    selections.push({
      id: normalizedOptions.effortDescriptor.id,
      value: effortValue,
    });
  }

  const fastModeDescriptor = getFastModeDescriptor(driver, normalizedOptions.descriptors);
  const fastModeValue = getProviderOptionCurrentValue(fastModeDescriptor);
  if (fastModeDescriptor?.type === "boolean" && typeof fastModeValue === "boolean") {
    selections.push({ id: fastModeDescriptor.id, value: fastModeValue });
  } else if (fastModeDescriptor?.type === "select" && typeof fastModeValue === "string") {
    selections.push({ id: fastModeDescriptor.id, value: fastModeValue });
  }

  return selections.length > 0 ? selections : undefined;
}

export function updateProviderSessionDefault(input: {
  readonly driver: ProviderDriverKind;
  readonly models: ReadonlyArray<ServerProviderModel>;
  readonly current?: ProviderSessionDefault | null;
  readonly change: ProviderSessionDefaultChange;
}): ProviderSessionDefault {
  const fallbackModel = getFallbackModel(input.models);
  const currentModel =
    input.current?.model ?? fallbackModel?.slug ?? getProviderDefaultModel(input.driver);
  const requestedModel =
    input.change.type === "model" ? input.change.value.trim() || currentModel : currentModel;
  const configuredServerModel = findSelectableModel(input.driver, input.models, requestedModel);
  const selectedServerModel =
    configuredServerModel ?? (input.change.type === "model" ? null : fallbackModel);
  const persistedModel =
    input.change.type === "model" && configuredServerModel
      ? configuredServerModel.slug
      : requestedModel;

  let selections = input.current?.options ? [...input.current.options] : [];
  const optionsModel =
    selectedServerModel ?? getInvariantProviderModel(input.driver, persistedModel, selections);
  const normalizeUpdatedSelections = (
    nextSelections: ReadonlyArray<ProviderOptionSelection>,
  ): NormalizedProviderOptions =>
    normalizeProviderOptions(
      input.driver,
      selectedServerModel ??
        getInvariantProviderModel(input.driver, persistedModel, nextSelections),
      nextSelections,
    );
  let normalizedOptions = normalizeProviderOptions(input.driver, optionsModel, selections);

  if (input.change.type === "effort" && normalizedOptions.effortDescriptor) {
    selections = replaceSelection(
      selections,
      {
        id: normalizedOptions.effortDescriptor.id,
        value: input.change.value,
      },
      PROVIDER_SESSION_EFFORT_OPTION_IDS,
    );
    normalizedOptions = normalizeUpdatedSelections(selections);
  } else if (input.change.type === "fastMode") {
    const fastModeDescriptor = getFastModeDescriptor(input.driver, normalizedOptions.descriptors);
    if (fastModeDescriptor?.type === "boolean") {
      selections = replaceSelection(selections, {
        id: fastModeDescriptor.id,
        value: input.change.value,
      });
      normalizedOptions = normalizeUpdatedSelections(selections);
    } else if (fastModeDescriptor?.type === "select") {
      const offValue = getFastModeOffValue(input.driver, fastModeDescriptor);
      const value = input.change.value
        ? getFastModeOnValue(input.driver, fastModeDescriptor)
        : offValue;
      if (typeof value === "string") {
        selections = replaceSelection(selections, {
          id: fastModeDescriptor.id,
          value,
        });
        normalizedOptions = normalizeUpdatedSelections(selections);
      }
    }
  }

  const persistedSelections = buildPersistedSelections(input.driver, normalizedOptions);
  return persistedSelections
    ? { model: persistedModel, options: persistedSelections }
    : { model: persistedModel };
}
