import {
  DEFAULT_MODEL,
  DEFAULT_MODEL_BY_PROVIDER,
  MODEL_SLUG_ALIASES_BY_PROVIDER,
  type ModelCapabilities,
  type ModelSelection,
  ProviderDriverKind,
  ProviderInstanceId,
  type ProviderOptionDescriptor,
  type ProviderOptionSelection,
} from "@coddy/contracts";

const DEFAULT_PROVIDER_DRIVER_KIND = ProviderDriverKind.make("codex");
const CODEX_PROVIDER_DRIVER_KIND = ProviderDriverKind.make("codex");
export const PROVIDER_EFFORT_OPTION_IDS = ["reasoningEffort", "effort", "reasoning"] as const;
const CODEX_REASONING_EFFORT_OPTION_ID = "reasoningEffort";
const DEFAULT_CODEX_REASONING_EFFORT = "medium";
const CODEX_REASONING_EFFORT_OPTIONS = [
  { id: "none", label: "None" },
  { id: "minimal", label: "Minimal" },
  { id: "low", label: "Low" },
  { id: "medium", label: "Medium", isDefault: true },
  { id: "high", label: "High" },
  { id: "xhigh", label: "Extra High" },
  { id: "max", label: "Max" },
  { id: "ultra", label: "Ultra" },
] as const;

export interface SelectableModelOption {
  slug: string;
  name: string;
}

export function createModelCapabilities(input: {
  optionDescriptors: ReadonlyArray<ProviderOptionDescriptor>;
}): ModelCapabilities {
  return {
    optionDescriptors: input.optionDescriptors.map(cloneDescriptor),
  };
}

function getRawSelectionValueById(
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
  id: string,
): string | boolean | undefined {
  const selection = selections?.find((candidate) => candidate.id === id);
  return selection?.value;
}

export function getProviderOptionSelectionValue(
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
  id: string,
): string | boolean | undefined {
  return getRawSelectionValueById(selections, id);
}

export function getProviderOptionStringSelectionValue(
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
  id: string,
): string | undefined {
  const value = getProviderOptionSelectionValue(selections, id);
  return typeof value === "string" ? value : undefined;
}

export function getProviderOptionBooleanSelectionValue(
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
  id: string,
): boolean | undefined {
  const value = getProviderOptionSelectionValue(selections, id);
  return typeof value === "boolean" ? value : undefined;
}

export function getModelSelectionOptionValue(
  modelSelection: ModelSelection | null | undefined,
  id: string,
): string | boolean | undefined {
  return getProviderOptionSelectionValue(modelSelection?.options, id);
}

export function getModelSelectionStringOptionValue(
  modelSelection: ModelSelection | null | undefined,
  id: string,
): string | undefined {
  return getProviderOptionStringSelectionValue(modelSelection?.options, id);
}

export function getModelSelectionBooleanOptionValue(
  modelSelection: ModelSelection | null | undefined,
  id: string,
): boolean | undefined {
  return getProviderOptionBooleanSelectionValue(modelSelection?.options, id);
}

function resolveDescriptorChoiceValue(
  descriptor: Extract<ProviderOptionDescriptor, { type: "select" }>,
  raw: string | null | undefined,
  preservePromptInjectedSelections: boolean,
): string | undefined {
  const trimmed = trimOrNull(raw);
  if (!trimmed) {
    return descriptor.currentValue ?? descriptor.options.find((option) => option.isDefault)?.id;
  }
  if (descriptor.options.length === 0) {
    return trimmed;
  }
  if (
    !preservePromptInjectedSelections &&
    descriptor.promptInjectedValues?.includes(trimmed) &&
    descriptor.options.some((option) => option.id === trimmed)
  ) {
    return descriptor.options.find((option) => option.isDefault)?.id;
  }
  if (descriptor.options.some((option) => option.id === trimmed)) {
    return trimmed;
  }
  return descriptor.currentValue ?? descriptor.options.find((option) => option.isDefault)?.id;
}

function cloneDescriptor(descriptor: ProviderOptionDescriptor): ProviderOptionDescriptor {
  return descriptor.type === "select"
    ? {
        ...descriptor,
        options: [...descriptor.options],
        ...(descriptor.promptInjectedValues
          ? { promptInjectedValues: [...descriptor.promptInjectedValues] }
          : {}),
      }
    : { ...descriptor };
}

function cloneSelection(selection: ProviderOptionSelection): ProviderOptionSelection {
  return { ...selection };
}

function selectedStringByIds(
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
  ids: ReadonlyArray<string>,
): string | undefined {
  const selection = selections?.find(
    (candidate) => ids.includes(candidate.id) && typeof candidate.value === "string",
  );
  return typeof selection?.value === "string" ? selection.value : undefined;
}

function uniqueSelectOptions(
  options: Extract<ProviderOptionDescriptor, { type: "select" }>["options"],
): Extract<ProviderOptionDescriptor, { type: "select" }>["options"] {
  const seen = new Set<string>();
  return options.filter((option) => {
    if (seen.has(option.id)) return false;
    seen.add(option.id);
    return true;
  });
}

function codexReasoningEffortDescriptor(
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
  metadata?: Pick<ProviderOptionDescriptor, "label" | "description">,
): ProviderOptionDescriptor {
  const selectedEffort =
    selectedStringByIds(selections, PROVIDER_EFFORT_OPTION_IDS) ?? DEFAULT_CODEX_REASONING_EFFORT;
  const options = CODEX_REASONING_EFFORT_OPTIONS.some((option) => option.id === selectedEffort)
    ? [...CODEX_REASONING_EFFORT_OPTIONS]
    : [...CODEX_REASONING_EFFORT_OPTIONS, { id: selectedEffort, label: selectedEffort }];
  return {
    id: CODEX_REASONING_EFFORT_OPTION_ID,
    label: metadata?.label ?? "Reasoning",
    ...(metadata?.description ? { description: metadata.description } : {}),
    type: "select",
    options,
    currentValue: selectedEffort,
  };
}

function withCodexReasoningEffortInvariant(
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
  selections: ReadonlyArray<ProviderOptionSelection> | null | undefined,
): ReadonlyArray<ProviderOptionDescriptor> {
  const effortDescriptors = descriptors.filter((descriptor) =>
    PROVIDER_EFFORT_OPTION_IDS.some((id) => id === descriptor.id),
  );
  const liveDescriptor = effortDescriptors.find(
    (descriptor): descriptor is Extract<ProviderOptionDescriptor, { type: "select" }> =>
      descriptor.type === "select" && descriptor.options.length > 0,
  );
  const remainingDescriptors = descriptors.filter(
    (descriptor) => !PROVIDER_EFFORT_OPTION_IDS.some((id) => id === descriptor.id),
  );

  if (!liveDescriptor) {
    return [
      codexReasoningEffortDescriptor(selections, effortDescriptors[0]),
      ...remainingDescriptors,
    ];
  }

  const options = uniqueSelectOptions(liveDescriptor.options);
  const selectedEffort = selectedStringByIds(selections, PROVIDER_EFFORT_OPTION_IDS);
  const liveCurrentValue = options.some((option) => option.id === liveDescriptor.currentValue)
    ? liveDescriptor.currentValue
    : undefined;
  const currentValue =
    (selectedEffort && options.some((option) => option.id === selectedEffort)
      ? selectedEffort
      : undefined) ??
    liveCurrentValue ??
    options.find((option) => option.isDefault)?.id ??
    options.find((option) => option.id === DEFAULT_CODEX_REASONING_EFFORT)?.id ??
    options[0]!.id;
  return [{ ...liveDescriptor, options, currentValue }, ...remainingDescriptors];
}

function withDescriptorCurrentValue(
  descriptor: ProviderOptionDescriptor,
  rawCurrentValue: string | boolean | undefined,
  preservePromptInjectedSelections: boolean,
): ProviderOptionDescriptor {
  if (descriptor.type === "boolean") {
    if (typeof rawCurrentValue === "boolean") {
      return {
        ...descriptor,
        currentValue: rawCurrentValue,
      };
    }
    return descriptor;
  }
  const currentValue =
    typeof rawCurrentValue === "string"
      ? resolveDescriptorChoiceValue(descriptor, rawCurrentValue, preservePromptInjectedSelections)
      : resolveDescriptorChoiceValue(
          descriptor,
          descriptor.currentValue,
          preservePromptInjectedSelections,
        );
  if (!currentValue) {
    const { currentValue: _unusedCurrentValue, ...rest } = descriptor;
    return rest;
  }
  return {
    ...descriptor,
    currentValue,
  };
}

export function getProviderOptionDescriptors(input: {
  caps: ModelCapabilities;
  selections?: ReadonlyArray<ProviderOptionSelection> | null | undefined;
  preservePromptInjectedSelections?: boolean;
}): ReadonlyArray<ProviderOptionDescriptor> {
  const { caps, selections, preservePromptInjectedSelections = false } = input;
  const baseDescriptors = (caps.optionDescriptors ?? []).map(cloneDescriptor);

  return baseDescriptors.map((descriptor) =>
    withDescriptorCurrentValue(
      descriptor,
      getRawSelectionValueById(selections, descriptor.id) ?? descriptor.currentValue,
      preservePromptInjectedSelections,
    ),
  );
}

export function getProviderCapabilityDescriptors(input: {
  provider: ProviderDriverKind;
  caps: ModelCapabilities;
  selections?: ReadonlyArray<ProviderOptionSelection> | null | undefined;
  preservePromptInjectedSelections?: boolean;
}): ReadonlyArray<ProviderOptionDescriptor> {
  const { provider, caps, selections, preservePromptInjectedSelections = false } = input;
  const liveDescriptors = caps.optionDescriptors ?? [];
  const hasCodexEffort = liveDescriptors.some((descriptor) =>
    PROVIDER_EFFORT_OPTION_IDS.some((id) => id === descriptor.id),
  );
  const hasSelectedEffort = selections?.some((selection) =>
    PROVIDER_EFFORT_OPTION_IDS.some((id) => id === selection.id),
  );
  const optionDescriptors =
    provider === CODEX_PROVIDER_DRIVER_KIND && (hasCodexEffort || hasSelectedEffort)
      ? withCodexReasoningEffortInvariant(liveDescriptors, selections)
      : liveDescriptors;
  return getProviderOptionDescriptors({
    caps: { ...caps, optionDescriptors },
    selections,
    preservePromptInjectedSelections,
  });
}

export function getProviderOptionCurrentValue(
  descriptor: ProviderOptionDescriptor | null | undefined,
): string | boolean | undefined {
  if (!descriptor) {
    return undefined;
  }
  if (descriptor.type === "boolean") {
    return descriptor.currentValue;
  }
  if (descriptor.currentValue) {
    return descriptor.currentValue;
  }
  return descriptor.options.find((option) => option.isDefault)?.id;
}

export function getProviderOptionCurrentLabel(
  descriptor: ProviderOptionDescriptor | null | undefined,
): string | undefined {
  if (!descriptor) {
    return undefined;
  }
  if (descriptor.type === "boolean") {
    return typeof descriptor.currentValue === "boolean"
      ? descriptor.currentValue
        ? "On"
        : "Off"
      : undefined;
  }
  const currentValue = getProviderOptionCurrentValue(descriptor);
  if (typeof currentValue !== "string") {
    return undefined;
  }
  return descriptor.options.find((option) => option.id === currentValue)?.label;
}

export function buildProviderOptionSelectionsFromDescriptors(
  descriptors: ReadonlyArray<ProviderOptionDescriptor> | null | undefined,
): Array<ProviderOptionSelection> | undefined {
  if (!descriptors || descriptors.length === 0) {
    return undefined;
  }

  const nextSelections: Array<ProviderOptionSelection> = [];

  for (const descriptor of descriptors) {
    const value = getProviderOptionCurrentValue(descriptor);
    if (typeof value === "string" || typeof value === "boolean") {
      nextSelections.push({ id: descriptor.id, value });
    }
  }

  return nextSelections.length > 0 ? nextSelections : undefined;
}

export function getModelSelectionOptionDescriptors(
  modelSelection: ModelSelection | null | undefined,
  caps?: ModelCapabilities | null | undefined,
): ReadonlyArray<ProviderOptionDescriptor> {
  if (!modelSelection) {
    return [];
  }
  if (!caps) {
    return [];
  }
  return getProviderOptionDescriptors({
    caps,
    selections: modelSelection.options,
  });
}

export function isClaudeUltrathinkPrompt(text: string | null | undefined): boolean {
  return typeof text === "string" && /\bultrathink\b/i.test(text);
}

export function normalizeModelSlug(
  model: string | null | undefined,
  provider: ProviderDriverKind = DEFAULT_PROVIDER_DRIVER_KIND,
): string | null {
  if (typeof model !== "string") {
    return null;
  }

  const trimmed = model.trim();
  if (!trimmed) {
    return null;
  }

  const aliases = MODEL_SLUG_ALIASES_BY_PROVIDER[provider] ?? {};
  const aliased = Object.prototype.hasOwnProperty.call(aliases, trimmed)
    ? aliases[trimmed]
    : undefined;
  return typeof aliased === "string" ? aliased : trimmed;
}

export function resolveSelectableModel(
  provider: ProviderDriverKind,
  value: string | null | undefined,
  options: ReadonlyArray<SelectableModelOption>,
): string | null {
  if (typeof value !== "string") {
    return null;
  }

  const trimmed = value.trim();
  if (!trimmed) {
    return null;
  }

  const direct = options.find((option) => option.slug === trimmed);
  if (direct) {
    return direct.slug;
  }

  const normalized = normalizeModelSlug(trimmed, provider);
  const matchingIdentity = options.find(
    (option) => normalizeModelSlug(option.slug, provider) === normalized,
  );
  if (matchingIdentity) {
    return matchingIdentity.slug;
  }

  const byName = options.find((option) => option.name.toLowerCase() === trimmed.toLowerCase());
  return byName?.slug ?? null;
}

function resolveModelSlug(model: string | null | undefined, provider: ProviderDriverKind): string {
  const normalized = normalizeModelSlug(model, provider);
  if (!normalized) {
    return DEFAULT_MODEL_BY_PROVIDER[provider] ?? DEFAULT_MODEL;
  }
  return normalized;
}

export function resolveModelSlugForProvider(
  provider: ProviderDriverKind,
  model: string | null | undefined,
): string {
  return resolveModelSlug(model, provider);
}

/** Trim a string, returning null for empty/missing values. */
export function trimOrNull<T extends string>(value: T | null | undefined): T | null {
  if (typeof value !== "string") return null;
  const trimmed = value.trim() as T;
  return trimmed || null;
}

function cloneSelections(
  selections: ReadonlyArray<ProviderOptionSelection>,
): Array<ProviderOptionSelection> {
  return selections.map(cloneSelection);
}

export function createModelSelection(
  instanceId: ProviderInstanceId,
  model: string,
  options?: ReadonlyArray<ProviderOptionSelection> | null,
): ModelSelection {
  const selections = options ? cloneSelections(options) : [];
  const base: ModelSelection = {
    instanceId,
    model,
  };
  return selections.length > 0 ? { ...base, options: selections } : base;
}

/**
 * Returns the effort value if it is a prompt-injected value according to
 * any select descriptor in the given capabilities, or null otherwise.
 *
 * Unlike a single `find`, this checks every descriptor so that the
 * correct descriptor's `promptInjectedValues` list is consulted even when
 * multiple select descriptors exist.
 */
export function resolvePromptInjectedEffort(
  caps: ModelCapabilities,
  rawEffort: string | null | undefined,
): string | null {
  const trimmed = trimOrNull(rawEffort);
  if (!trimmed) return null;
  const descriptors = getProviderOptionDescriptors({ caps });
  for (const descriptor of descriptors) {
    if (descriptor.type === "select" && descriptor.promptInjectedValues?.includes(trimmed)) {
      return trimmed;
    }
  }
  return null;
}

export function applyClaudePromptEffortPrefix(
  text: string,
  effort: string | null | undefined,
): string {
  const trimmed = text.trim();
  if (!trimmed) {
    return trimmed;
  }
  if (effort !== "ultrathink") {
    return trimmed;
  }
  if (trimmed.startsWith("Ultrathink:")) {
    return trimmed;
  }
  return `Ultrathink:\n${trimmed}`;
}
