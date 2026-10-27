import {
  type ProviderDriverKind,
  type ProviderInstanceId,
  type ProviderOptionSelection,
  type ScopedThreadRef,
  type ServerProviderModel,
} from "@coddy/contracts";
import {
  buildProviderOptionSelectionsFromDescriptors,
  getProviderCapabilityDescriptors,
  getProviderOptionCurrentValue,
  getProviderOptionStringSelectionValue,
  isClaudeUltrathinkPrompt,
  resolvePromptInjectedEffort,
} from "@coddy/shared/model";
import type { DraftId } from "../../composerDraftStore";
import { getProviderModelCapabilities } from "../../providerModels";
import { ComposerTraitControls, findProviderEffortDescriptor } from "./TraitsPicker";
import { getFastModeDescriptor } from "@coddy/shared/providerSessionDefaults";

const CODEX_PROVIDER = "codex";

export type ComposerControlAvailability =
  | { state: "supported" }
  | { state: "unknown"; reason: string }
  | { state: "unsupported"; reason: string };

export type ComposerProviderStateInput = {
  provider: ProviderDriverKind;
  model: string;
  models: ReadonlyArray<ServerProviderModel>;
  promptInjectionState?: ComposerPromptInjectionState;
  modelOptions: ReadonlyArray<ProviderOptionSelection> | null | undefined;
};

export type ComposerPromptInjectionState = "none" | "ultrathink";

export type ComposerProviderState = {
  provider: ProviderDriverKind;
  promptEffort: string | null;
  modelOptionsForDispatch: ReadonlyArray<ProviderOptionSelection> | undefined;
  composerFrameClassName?: string;
  composerSurfaceClassName?: string;
  modelPickerIconClassName?: string;
};

type TraitsRenderInput = {
  provider: ProviderDriverKind;
  instanceId?: ProviderInstanceId;
  threadRef?: ScopedThreadRef;
  draftId?: DraftId;
  model: string;
  models: ReadonlyArray<ServerProviderModel>;
  providerSnapshotLoaded?: boolean;
  modelOptions: ReadonlyArray<ProviderOptionSelection> | undefined;
  prompt: string;
  onPromptChange: (prompt: string) => void;
  onModelOptionsChange?: (
    nextOptions: ReadonlyArray<ProviderOptionSelection> | undefined,
  ) => void | Promise<void>;
};

function controlAvailability(input: {
  descriptorPresent: boolean;
  provider: ProviderDriverKind;
  model: string;
  models: ReadonlyArray<ServerProviderModel>;
  providerSnapshotLoaded: boolean;
  label: string;
}): ComposerControlAvailability {
  if (
    !input.providerSnapshotLoaded ||
    !input.models.some((candidate) => candidate.slug === input.model)
  ) {
    return { state: "unknown", reason: `${input.label} availability is still loading.` };
  }
  if (input.descriptorPresent) return { state: "supported" };
  const providerLabel = input.provider.replace(/([a-z])([A-Z])/g, "$1 $2").replace(/[_-]+/g, " ");
  const modelLabel =
    input.models.find((candidate) => candidate.slug === input.model)?.name ?? input.model;
  return {
    state: "unsupported",
    reason: `${input.label} is not supported by ${modelLabel} through ${providerLabel}.`,
  };
}

export function getComposerPromptInjectionState(prompt: string): ComposerPromptInjectionState {
  return isClaudeUltrathinkPrompt(prompt) ? "ultrathink" : "none";
}

export function getComposerProviderState(input: ComposerProviderStateInput): ComposerProviderState {
  const { provider, model, models, modelOptions, promptInjectionState = "none" } = input;
  const caps = getProviderModelCapabilities(models, model, provider);
  const descriptors = getProviderCapabilityDescriptors({
    provider,
    caps,
    selections: modelOptions,
  });
  const primarySelectDescriptor = findProviderEffortDescriptor(descriptors);
  const primaryValue = getProviderOptionCurrentValue(primarySelectDescriptor ?? null);
  const rawPrimaryValue = primarySelectDescriptor
    ? getProviderOptionStringSelectionValue(modelOptions, primarySelectDescriptor.id)
    : undefined;
  const promptInjectedEffort = resolvePromptInjectedEffort(caps, rawPrimaryValue);
  const promptEffort =
    promptInjectedEffort ?? (typeof primaryValue === "string" ? primaryValue : null);
  const ultrathinkActive =
    (primarySelectDescriptor?.promptInjectedValues?.length ?? 0) > 0 &&
    (promptInjectedEffort === "ultrathink" || promptInjectionState === "ultrathink");

  return {
    provider,
    promptEffort,
    modelOptionsForDispatch: buildProviderOptionSelectionsFromDescriptors(descriptors),
    ...(ultrathinkActive
      ? {
          composerFrameClassName: "ultrathink-frame",
          composerSurfaceClassName: "shadow-[0_0_0_1px_rgba(255,255,255,0.07)_inset]",
          modelPickerIconClassName: "ultrathink-chroma",
        }
      : {}),
  };
}

export function renderComposerTraitControls(input: TraitsRenderInput) {
  const {
    provider,
    instanceId,
    threadRef,
    draftId,
    model,
    models,
    modelOptions,
    prompt,
    onPromptChange,
    onModelOptionsChange,
  } = input;
  const hasTarget = threadRef !== undefined || draftId !== undefined;
  const modelOptionsForRender =
    provider === CODEX_PROVIDER
      ? getComposerProviderState({ provider, model, models, modelOptions }).modelOptionsForDispatch
      : modelOptions;
  if (!hasTarget) {
    return null;
  }
  const snapshotLoaded = input.providerSnapshotLoaded ?? models.length > 0;
  const descriptors = getProviderCapabilityDescriptors({
    provider,
    caps: getProviderModelCapabilities(models, model, provider),
    selections: modelOptionsForRender,
  });
  const fastDescriptor = getFastModeDescriptor(provider, descriptors);
  const effortDescriptor = findProviderEffortDescriptor(descriptors);
  return (
    <ComposerTraitControls
      provider={provider}
      {...(instanceId ? { instanceId } : {})}
      models={models}
      {...(threadRef ? { threadRef } : {})}
      {...(draftId ? { draftId } : {})}
      model={model}
      modelOptions={modelOptionsForRender}
      fastAvailability={controlAvailability({
        descriptorPresent: fastDescriptor !== null,
        provider,
        model,
        models,
        providerSnapshotLoaded: snapshotLoaded,
        label: "Fast mode",
      })}
      effortAvailability={controlAvailability({
        descriptorPresent: effortDescriptor !== null,
        provider,
        model,
        models,
        providerSnapshotLoaded: snapshotLoaded,
        label: "Reasoning effort",
      })}
      prompt={prompt}
      onPromptChange={onPromptChange}
      {...(onModelOptionsChange ? { onModelOptionsChange } : {})}
    />
  );
}
