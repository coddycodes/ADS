import {
  type ProviderDriverKind,
  type ProviderInstanceId,
  type ProviderOptionDescriptor,
  type ProviderOptionSelection,
  type ScopedThreadRef,
  type ServerProviderModel,
} from "@bibcode/contracts";
import {
  applyClaudePromptEffortPrefix,
  buildProviderOptionSelectionsFromDescriptors,
  getProviderCapabilityDescriptors,
  getProviderOptionCurrentLabel,
  getProviderOptionCurrentValue,
  getProviderOptionStringSelectionValue,
  isClaudeUltrathinkPrompt,
  PROVIDER_EFFORT_OPTION_IDS,
  resolvePromptInjectedEffort,
} from "@bibcode/shared/model";
import {
  getFastModeDescriptor,
  getFastModeOffValue,
  getFastModeOnValue,
} from "@bibcode/shared/providerSessionDefaults";
import { memo, useCallback, useEffect, useRef, useState } from "react";
import type { VariantProps } from "class-variance-authority";
import { ChevronDownIcon, LoaderCircleIcon, ZapIcon } from "lucide-react";
import { Button, buttonVariants } from "../ui/button";
import {
  Menu,
  MenuGroup,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator as MenuDivider,
  MenuTrigger,
} from "../ui/menu";
import { useComposerDraftStore, DraftId } from "../../composerDraftStore";
import { getProviderModelCapabilities } from "../../providerModels";
import { cn } from "~/lib/utils";
import { toastManager } from "../ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

type ProviderOptions = ReadonlyArray<ProviderOptionSelection>;
type ComposerControlAvailability =
  | { state: "supported" }
  | { state: "unknown"; reason: string }
  | { state: "unsupported"; reason: string };

type CommitModelOptions = (nextOptions: ProviderOptions | undefined) => void | Promise<void>;

const ACTIVE_CONTROL_CLASSNAME =
  "border-foreground/12 bg-foreground/10 text-foreground hover:border-foreground/18 hover:bg-foreground/15 hover:text-foreground dark:border-foreground/18 dark:bg-foreground/14 dark:hover:border-foreground/24 dark:hover:bg-foreground/20 [&_svg]:!text-foreground";

type TraitsPersistence = {
  threadRef?: ScopedThreadRef;
  draftId?: DraftId;
  onModelOptionsChange?: CommitModelOptions;
};

const ULTRATHINK_PROMPT_PREFIX = "Ultrathink:\n";

function replaceDescriptorCurrentValue(
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
  descriptorId: string,
  currentValue: string | boolean | undefined,
): ReadonlyArray<ProviderOptionDescriptor> {
  return descriptors.map((descriptor) =>
    descriptor.id !== descriptorId
      ? descriptor
      : descriptor.type === "boolean"
        ? {
            ...descriptor,
            ...(typeof currentValue === "boolean" ? { currentValue } : {}),
          }
        : {
            ...descriptor,
            ...(typeof currentValue === "string" ? { currentValue } : {}),
          },
  );
}

type UpdateDescriptor = (
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
  descriptorId: string,
  currentValue: string | boolean | undefined,
) => Promise<void>;

export interface ProviderOptionUpdater {
  readonly pendingDescriptorIds: ReadonlySet<string>;
  readonly updateDescriptor: UpdateDescriptor;
}

export function useProviderOptionUpdater(
  provider: ProviderDriverKind,
  instanceId: ProviderInstanceId | undefined,
  model: string | null | undefined,
  persistence: TraitsPersistence,
): ProviderOptionUpdater {
  const setProviderModelOptions = useComposerDraftStore((store) => store.setProviderModelOptions);
  const mountedRef = useRef(true);
  // Each provider, instance and model gets a fresh pending scope. An update that settles after
  // the scope changed leaves the new scope's pending markers alone.
  const [pendingScope, setPendingScope] = useState(() => ({ provider, instanceId, model }));
  const committedPendingScopeRef = useRef(pendingScope);
  const pendingDescriptorIdsRef = useRef({ scope: pendingScope, ids: new Set<string>() });
  const [pendingDescriptorIds, setPendingDescriptorIds] = useState<ReadonlySet<string>>(new Set());

  if (
    pendingScope.provider !== provider ||
    pendingScope.instanceId !== instanceId ||
    pendingScope.model !== model
  ) {
    setPendingScope({ provider, instanceId, model });
    setPendingDescriptorIds(new Set());
  }

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  useEffect(() => {
    committedPendingScopeRef.current = pendingScope;
  }, [pendingScope]);

  const updateDescriptor: UpdateDescriptor = useCallback(
    async (descriptors, descriptorId, currentValue) => {
      const scope = committedPendingScopeRef.current;
      if (pendingDescriptorIdsRef.current.scope !== scope) {
        pendingDescriptorIdsRef.current = { scope, ids: new Set() };
      }
      const pending = pendingDescriptorIdsRef.current;
      if (pending.ids.size > 0) return;
      pending.ids.add(descriptorId);
      setPendingDescriptorIds(new Set(pending.ids));
      try {
        const nextOptions = buildProviderOptionSelectionsFromDescriptors(
          replaceDescriptorCurrentValue(descriptors, descriptorId, currentValue),
        );
        if (persistence.onModelOptionsChange) {
          await persistence.onModelOptionsChange(nextOptions);
          return;
        }
        const threadTarget = persistence.threadRef ?? persistence.draftId;
        if (!threadTarget) return;
        setProviderModelOptions(threadTarget, provider, nextOptions, {
          ...(instanceId ? { instanceId } : {}),
          model,
          persistSticky: true,
        });
      } catch (error) {
        toastManager.add({
          type: "error",
          title: "Could not update provider option",
          description:
            error instanceof Error ? error.message : "The provider rejected this option.",
        });
      } finally {
        if (committedPendingScopeRef.current === scope) {
          pending.ids.delete(descriptorId);
          if (mountedRef.current) {
            setPendingDescriptorIds(new Set(pending.ids));
          }
        }
      }
    },
    [instanceId, model, persistence, provider, setProviderModelOptions],
  );

  return { pendingDescriptorIds, updateDescriptor };
}

function getDescriptorStringValue(
  descriptor: Extract<ProviderOptionDescriptor, { type: "select" }> | null,
): string | null {
  if (!descriptor) {
    return null;
  }
  const value = getProviderOptionCurrentValue(descriptor);
  return typeof value === "string" ? value : null;
}

export function findProviderEffortDescriptor(
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
): Extract<ProviderOptionDescriptor, { type: "select" }> | null {
  for (const id of PROVIDER_EFFORT_OPTION_IDS) {
    const descriptor = descriptors.find(
      (candidate): candidate is Extract<ProviderOptionDescriptor, { type: "select" }> =>
        candidate.id === id && candidate.type === "select",
    );
    if (descriptor) {
      return descriptor;
    }
  }
  return null;
}

function isHighestEffortSelection(
  descriptor: Extract<ProviderOptionDescriptor, { type: "select" }> | null,
  value: string,
): boolean {
  if (!descriptor || value.length === 0 || descriptor.options.length === 0) {
    return false;
  }
  return descriptor.options.at(-1)?.id === value;
}

function getSelectedTraits(
  provider: ProviderDriverKind,
  models: ReadonlyArray<ServerProviderModel>,
  model: string | null | undefined,
  prompt: string,
  modelOptions: ProviderOptions | null | undefined,
  allowPromptInjectedEffort: boolean,
) {
  const caps = getProviderModelCapabilities(models, model, provider);
  const descriptors = getProviderCapabilityDescriptors({
    provider,
    caps,
    selections: modelOptions,
  });
  const selectDescriptors = descriptors.filter(
    (descriptor): descriptor is Extract<ProviderOptionDescriptor, { type: "select" }> =>
      descriptor.type === "select",
  );
  const booleanDescriptors = descriptors.filter(
    (descriptor): descriptor is Extract<ProviderOptionDescriptor, { type: "boolean" }> =>
      descriptor.type === "boolean",
  );
  const primarySelectDescriptor = findProviderEffortDescriptor(descriptors);
  const contextWindowDescriptor =
    selectDescriptors.find((descriptor) => descriptor.id === "contextWindow") ?? null;
  const agentDescriptor = selectDescriptors.find((descriptor) => descriptor.id === "agent") ?? null;
  const fastModeDescriptor = getFastModeDescriptor(provider, descriptors);
  const thinkingDescriptor =
    booleanDescriptors.find((descriptor) => descriptor.id === "thinking") ?? null;

  const rawPrimaryValue = primarySelectDescriptor
    ? getProviderOptionStringSelectionValue(modelOptions, primarySelectDescriptor.id)
    : undefined;
  const rawPromptInjectedEffort = allowPromptInjectedEffort
    ? resolvePromptInjectedEffort(caps, rawPrimaryValue)
    : null;

  // Prompt-controlled effort (e.g. ultrathink in prompt text)
  const ultrathinkPromptControlled =
    allowPromptInjectedEffort &&
    (primarySelectDescriptor?.promptInjectedValues?.length ?? 0) > 0 &&
    isClaudeUltrathinkPrompt(prompt);

  // Check if "ultrathink" appears in the body text (not just our prefix)
  const ultrathinkInBodyText =
    ultrathinkPromptControlled && isClaudeUltrathinkPrompt(prompt.replace(/^Ultrathink:\s*/i, ""));
  const selectedPromptInjectedEffort = ultrathinkPromptControlled
    ? "ultrathink"
    : rawPromptInjectedEffort;
  const effort =
    selectedPromptInjectedEffort ?? getDescriptorStringValue(primarySelectDescriptor) ?? null;
  const thinkingEnabled =
    typeof thinkingDescriptor?.currentValue === "boolean" ? thinkingDescriptor.currentValue : null;
  const fastModeEnabled =
    fastModeDescriptor?.type === "boolean"
      ? fastModeDescriptor.currentValue === true
      : getDescriptorStringValue(fastModeDescriptor ?? null) ===
        getFastModeOnValue(provider, fastModeDescriptor);
  const contextWindow = getDescriptorStringValue(contextWindowDescriptor);
  const selectedAgent = getDescriptorStringValue(agentDescriptor);
  const selectedAgentLabel = agentDescriptor
    ? getProviderOptionCurrentLabel(agentDescriptor)
    : null;

  return {
    caps,
    descriptors,
    selectDescriptors,
    booleanDescriptors,
    primarySelectDescriptor,
    contextWindowDescriptor,
    agentDescriptor,
    fastModeDescriptor,
    thinkingDescriptor,
    effort,
    thinkingEnabled,
    fastModeEnabled,
    contextWindow,
    selectedPromptInjectedEffort,
    ultrathinkPromptControlled,
    ultrathinkInBodyText,
    selectedAgent,
    selectedAgentLabel,
  };
}

function getTraitsSectionVisibility(input: {
  provider: ProviderDriverKind;
  models: ReadonlyArray<ServerProviderModel>;
  model: string | null | undefined;
  prompt: string;
  modelOptions: ProviderOptions | null | undefined;
  allowPromptInjectedEffort?: boolean;
}) {
  const selected = getSelectedTraits(
    input.provider,
    input.models,
    input.model,
    input.prompt,
    input.modelOptions,
    input.allowPromptInjectedEffort ?? true,
  );

  const showEffort = selected.primarySelectDescriptor !== null;
  const showThinking = selected.thinkingDescriptor !== null;
  const showFastMode = selected.fastModeDescriptor !== null;
  const showContextWindow = selected.contextWindowDescriptor !== null;
  const showAgent = selected.agentDescriptor !== null;

  return {
    ...selected,
    showEffort,
    showThinking,
    showFastMode,
    showContextWindow,
    showAgent,
    hasAnyControls: showEffort || showThinking || showFastMode || showContextWindow || showAgent,
  };
}

export function shouldRenderTraitsControls(input: {
  provider: ProviderDriverKind;
  models: ReadonlyArray<ServerProviderModel>;
  model: string | null | undefined;
  prompt: string;
  modelOptions: ProviderOptions | null | undefined;
  allowPromptInjectedEffort?: boolean;
}): boolean {
  return getTraitsSectionVisibility(input).hasAnyControls;
}

export function shouldRenderComposerTraitControls(input: {
  provider: ProviderDriverKind;
  models: ReadonlyArray<ServerProviderModel>;
  model: string | null | undefined;
  prompt: string;
  modelOptions: ProviderOptions | null | undefined;
  allowPromptInjectedEffort?: boolean;
}): boolean {
  const selected = getTraitsSectionVisibility(input);
  return selected.fastModeDescriptor !== null || selected.primarySelectDescriptor !== null;
}

function handleTraitSelectChange(input: {
  descriptor: Extract<ProviderOptionDescriptor, { type: "select" }>;
  value: string;
  descriptors: ReadonlyArray<ProviderOptionDescriptor>;
  primarySelectDescriptor: Extract<ProviderOptionDescriptor, { type: "select" }> | null;
  ultrathinkPromptControlled: boolean;
  ultrathinkInBodyText: boolean;
  prompt: string;
  onPromptChange: (prompt: string) => void;
  updateDescriptor: UpdateDescriptor;
}) {
  const {
    descriptor,
    value,
    descriptors,
    primarySelectDescriptor,
    ultrathinkPromptControlled,
    ultrathinkInBodyText,
    prompt,
    onPromptChange,
    updateDescriptor,
  } = input;
  if (!value) return;
  if (descriptor.promptInjectedValues?.includes(value)) {
    onPromptChange(
      prompt.trim().length === 0
        ? ULTRATHINK_PROMPT_PREFIX
        : applyClaudePromptEffortPrefix(prompt, "ultrathink"),
    );
    return;
  }
  if (ultrathinkInBodyText && descriptor.id === primarySelectDescriptor?.id) return;
  if (ultrathinkPromptControlled && descriptor.id === primarySelectDescriptor?.id) {
    onPromptChange(prompt.replace(/^Ultrathink:\s*/i, ""));
  }
  updateDescriptor(descriptors, descriptor.id, value);
}

export interface TraitsMenuContentProps {
  provider: ProviderDriverKind;
  instanceId?: ProviderInstanceId;
  models: ReadonlyArray<ServerProviderModel>;
  model: string | null | undefined;
  prompt: string;
  onPromptChange: (prompt: string) => void;
  modelOptions?: ProviderOptions | null | undefined;
  allowPromptInjectedEffort?: boolean;
  triggerVariant?: VariantProps<typeof buttonVariants>["variant"];
  triggerClassName?: string;
  descriptorIds?: ReadonlyArray<string>;
  optionUpdater?: ProviderOptionUpdater;
}

export const TraitsMenuContent = memo(function TraitsMenuContentImpl({
  provider,
  instanceId,
  models,
  model,
  prompt,
  onPromptChange,
  modelOptions,
  allowPromptInjectedEffort = true,
  descriptorIds,
  optionUpdater,
  ...persistence
}: TraitsMenuContentProps & TraitsPersistence) {
  const ownedUpdater = useProviderOptionUpdater(provider, instanceId, model, persistence);
  const { pendingDescriptorIds, updateDescriptor } = optionUpdater ?? ownedUpdater;
  const isPending = pendingDescriptorIds.size > 0;
  const {
    descriptors,
    selectDescriptors,
    booleanDescriptors,
    primarySelectDescriptor,
    selectedPromptInjectedEffort,
    ultrathinkPromptControlled,
    ultrathinkInBodyText,
  } = getTraitsSectionVisibility({
    provider,
    models,
    model,
    prompt,
    modelOptions,
    allowPromptInjectedEffort,
  });
  const visibleSelectDescriptors = descriptorIds
    ? selectDescriptors.filter((descriptor) => descriptorIds.includes(descriptor.id))
    : selectDescriptors;
  const visibleBooleanDescriptors = descriptorIds
    ? booleanDescriptors.filter((descriptor) => descriptorIds.includes(descriptor.id))
    : booleanDescriptors;

  if (visibleSelectDescriptors.length === 0 && visibleBooleanDescriptors.length === 0) {
    return null;
  }

  return (
    <>
      {visibleSelectDescriptors.map((descriptor, index) => {
        const selectedValue =
          selectedPromptInjectedEffort && descriptor.id === primarySelectDescriptor?.id
            ? selectedPromptInjectedEffort
            : (getDescriptorStringValue(descriptor) ?? "");
        const isPrimaryEffortDescriptor = descriptor.id === primarySelectDescriptor?.id;
        const highestEffortSelected =
          isPrimaryEffortDescriptor &&
          isHighestEffortSelection(primarySelectDescriptor, selectedValue);

        return (
          <div key={descriptor.id}>
            {index > 0 ? <MenuDivider /> : null}
            <MenuGroup>
              <div className="px-2 pt-1.5 pb-1 font-medium text-muted-foreground text-xs">
                {descriptor.label}
              </div>
              {ultrathinkInBodyText && isPrimaryEffortDescriptor ? (
                <div className="px-2 pb-1.5 text-muted-foreground/80 text-xs">
                  Your prompt contains &quot;ultrathink&quot; in the text. Remove it to change this
                  option.
                </div>
              ) : null}
              <MenuRadioGroup
                value={selectedValue}
                onValueChange={(value) => {
                  if (isPending) return;
                  handleTraitSelectChange({
                    descriptor,
                    value,
                    descriptors,
                    primarySelectDescriptor,
                    ultrathinkPromptControlled,
                    ultrathinkInBodyText,
                    prompt,
                    onPromptChange,
                    updateDescriptor,
                  });
                }}
              >
                {descriptor.options.map((option) => (
                  <MenuRadioItem
                    key={option.id}
                    value={option.id}
                    disabled={isPending || (ultrathinkInBodyText && isPrimaryEffortDescriptor)}
                  >
                    <span
                      data-effort-title={isPrimaryEffortDescriptor ? "true" : undefined}
                      className={cn(
                        highestEffortSelected && option.id === selectedValue && "text-destructive",
                      )}
                    >
                      {option.label}
                      {option.isDefault ? " (default)" : ""}
                    </span>
                  </MenuRadioItem>
                ))}
              </MenuRadioGroup>
            </MenuGroup>
          </div>
        );
      })}
      {visibleBooleanDescriptors.map((descriptor, index) => (
        <div key={descriptor.id}>
          {index > 0 || visibleSelectDescriptors.length > 0 ? <MenuDivider /> : null}
          <MenuGroup>
            <div className="px-2 py-1.5 font-medium text-muted-foreground text-xs">
              {descriptor.label}
            </div>
            <MenuRadioGroup
              value={descriptor.currentValue === true ? "on" : "off"}
              onValueChange={(value) => {
                if (!isPending) {
                  void updateDescriptor(descriptors, descriptor.id, value === "on");
                }
              }}
            >
              <MenuRadioItem value="on" disabled={isPending}>
                On
              </MenuRadioItem>
              <MenuRadioItem value="off" disabled={isPending}>
                Off
              </MenuRadioItem>
            </MenuRadioGroup>
          </MenuGroup>
        </div>
      ))}
    </>
  );
});

function EffortLevelIcon({ level, levels }: { level: number; levels: number }) {
  const barCount = Math.max(1, levels);
  return (
    <span aria-hidden="true" className="flex h-4 items-end gap-0.5">
      {Array.from({ length: barCount }, (_, index) => (
        <span
          key={index}
          className={cn("w-0.5 rounded-sm", index < level ? "bg-current" : "bg-current/30")}
          style={{ height: barCount === 1 ? 14 : 4 + Math.round((10 * index) / (barCount - 1)) }}
        />
      ))}
    </span>
  );
}

export const ComposerTraitControls = memo(function ComposerTraitControls({
  provider,
  instanceId,
  models,
  model,
  prompt,
  onPromptChange,
  modelOptions,
  allowPromptInjectedEffort = true,
  fastAvailability,
  effortAvailability,
  ...persistence
}: TraitsMenuContentProps &
  TraitsPersistence & {
    fastAvailability?: ComposerControlAvailability;
    effortAvailability?: ComposerControlAvailability;
  }) {
  const { pendingDescriptorIds, updateDescriptor } = useProviderOptionUpdater(
    provider,
    instanceId,
    model,
    persistence,
  );
  const isPending = pendingDescriptorIds.size > 0;
  const {
    descriptors,
    primarySelectDescriptor,
    fastModeDescriptor,
    fastModeEnabled,
    selectedPromptInjectedEffort,
  } = getTraitsSectionVisibility({
    provider,
    models,
    model,
    prompt,
    modelOptions,
    allowPromptInjectedEffort,
  });
  const effortValue =
    selectedPromptInjectedEffort ?? getDescriptorStringValue(primarySelectDescriptor) ?? "";
  const effortOptionIndex =
    primarySelectDescriptor?.options.findIndex((option) => option.id === effortValue) ?? -1;
  const effortLevelCount = primarySelectDescriptor?.options.length ?? 1;
  const effortLevel = Math.max(1, Math.min(effortLevelCount, effortOptionIndex + 1));
  const effortLabel =
    primarySelectDescriptor?.options.find((option) => option.id === effortValue)?.label ??
    primarySelectDescriptor?.label;
  const effortIsHighest = isHighestEffortSelection(primarySelectDescriptor, effortValue);

  const resolvedFastAvailability =
    fastAvailability ??
    (fastModeDescriptor
      ? { state: "supported" }
      : { state: "unsupported", reason: "Fast mode is not supported by the selected model." });
  const resolvedEffortAvailability =
    effortAvailability ??
    (primarySelectDescriptor
      ? { state: "supported" }
      : {
          state: "unsupported",
          reason: "Reasoning effort is not supported by the selected model.",
        });
  const fastOffValue = getFastModeOffValue(provider, fastModeDescriptor);
  const fastOnValue = getFastModeOnValue(provider, fastModeDescriptor);
  const fastOperable =
    resolvedFastAvailability.state === "supported" &&
    !isPending &&
    fastModeDescriptor &&
    fastOffValue !== null &&
    fastOnValue !== null;
  const effortOperable =
    resolvedEffortAvailability.state === "supported" && !isPending && primarySelectDescriptor;
  const fastLabel = isPending
    ? "Applying fast mode"
    : resolvedFastAvailability.state === "supported"
      ? fastModeEnabled
        ? "Disable fast mode"
        : "Enable fast mode"
      : resolvedFastAvailability.reason;
  const effortTooltip = isPending
    ? "Applying reasoning effort"
    : resolvedEffortAvailability.state === "supported"
      ? `Reasoning effort: ${effortLabel ?? "Unknown"}`
      : resolvedEffortAvailability.reason;
  const effortButton = (
    <Button
      type="button"
      size="sm"
      variant="ghost"
      className={cn(
        "h-7 px-1.5",
        resolvedEffortAvailability.state === "supported"
          ? !isPending && effortIsHighest
            ? "text-destructive hover:text-destructive"
            : "text-foreground/80 hover:text-foreground"
          : "border border-input bg-background text-muted-foreground/70",
      )}
      aria-label={effortTooltip}
      aria-disabled={!effortOperable}
      aria-busy={isPending}
    >
      {isPending ? (
        <LoaderCircleIcon aria-hidden="true" className="size-3.5 animate-spin" />
      ) : (
        <EffortLevelIcon level={effortLevel} levels={effortLevelCount} />
      )}
    </Button>
  );

  return (
    <div className="flex items-center gap-0.5">
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              type="button"
              size="sm"
              variant="ghost"
              className={cn(
                "h-7 gap-1 px-1.5",
                resolvedFastAvailability.state === "supported"
                  ? fastModeEnabled
                    ? ACTIVE_CONTROL_CLASSNAME
                    : "text-muted-foreground/70 hover:text-foreground/80"
                  : "border border-input bg-background text-muted-foreground/70",
              )}
              aria-label={fastLabel}
              aria-pressed={fastModeEnabled}
              aria-disabled={!fastOperable}
              aria-busy={isPending}
              onClick={() => {
                if (!fastOperable) return;
                void updateDescriptor(
                  descriptors,
                  fastModeDescriptor.id,
                  fastModeDescriptor.type === "boolean"
                    ? !fastModeEnabled
                    : fastModeEnabled
                      ? fastOffValue
                      : fastOnValue,
                );
              }}
            >
              {isPending ? (
                <LoaderCircleIcon aria-hidden="true" className="size-3.5 animate-spin" />
              ) : (
                <ZapIcon aria-hidden="true" className="size-3.5" />
              )}
              <span className="hidden lg:inline">Fast</span>
            </Button>
          }
        />
        <TooltipPopup side="top">{fastLabel}</TooltipPopup>
      </Tooltip>
      {effortOperable ? (
        <Menu>
          <Tooltip>
            <TooltipTrigger render={<MenuTrigger render={effortButton} />} />
            <TooltipPopup side="top">{effortTooltip}</TooltipPopup>
          </Tooltip>
          <MenuPopup align="start">
            <TraitsMenuContent
              provider={provider}
              {...(instanceId ? { instanceId } : {})}
              models={models}
              model={model}
              prompt={prompt}
              onPromptChange={onPromptChange}
              modelOptions={modelOptions}
              allowPromptInjectedEffort={allowPromptInjectedEffort}
              descriptorIds={[primarySelectDescriptor.id]}
              optionUpdater={{ pendingDescriptorIds, updateDescriptor }}
              {...persistence}
            />
          </MenuPopup>
        </Menu>
      ) : (
        <Tooltip>
          <TooltipTrigger render={effortButton} />
          <TooltipPopup side="top">{effortTooltip}</TooltipPopup>
        </Tooltip>
      )}
    </div>
  );
});

export const TraitsPicker = memo(function TraitsPicker({
  provider,
  instanceId,
  models,
  model,
  prompt,
  onPromptChange,
  modelOptions,
  allowPromptInjectedEffort = true,
  triggerVariant,
  triggerClassName,
  ...persistence
}: TraitsMenuContentProps & TraitsPersistence) {
  const [isMenuOpen, setIsMenuOpen] = useState(false);
  const { descriptors, primarySelectDescriptor, selectedPromptInjectedEffort } =
    getTraitsSectionVisibility({
      provider,
      models,
      model,
      prompt,
      modelOptions,
      allowPromptInjectedEffort,
    });
  if (
    !shouldRenderTraitsControls({
      provider,
      models,
      model,
      prompt,
      modelOptions,
      allowPromptInjectedEffort,
    })
  ) {
    return null;
  }

  const triggerLabels: Array<string> = [];
  for (const descriptor of descriptors) {
    const label =
      descriptor.type === "select" &&
      selectedPromptInjectedEffort &&
      descriptor.id === primarySelectDescriptor?.id
        ? descriptor.options.find((option) => option.id === selectedPromptInjectedEffort)?.label
        : descriptor.type === "boolean"
          ? descriptor.id === "fastMode"
            ? descriptor.currentValue === true
              ? "Fast"
              : "Normal"
            : `${descriptor.label} ${descriptor.currentValue === true ? "On" : "Off"}`
          : getProviderOptionCurrentLabel(descriptor);
    if (typeof label === "string" && label.length > 0) {
      triggerLabels.push(label);
    }
  }
  const triggerLabel = triggerLabels.join(" · ");

  const isCodexStyle = provider === "codex";

  return (
    <Menu
      open={isMenuOpen}
      onOpenChange={(open) => {
        setIsMenuOpen(open);
      }}
    >
      <MenuTrigger
        render={
          <Button
            size="sm"
            variant={triggerVariant ?? "ghost"}
            className={cn(
              isCodexStyle
                ? "min-w-0 max-w-40 shrink justify-start overflow-hidden whitespace-nowrap px-2 text-muted-foreground/70 hover:text-foreground/80 sm:max-w-48 sm:px-3 [&_svg]:mx-0"
                : "shrink-0 whitespace-nowrap px-2 text-muted-foreground/70 hover:text-foreground/80 sm:px-3",
              triggerClassName,
            )}
          />
        }
      >
        {isCodexStyle ? (
          <span className="flex min-w-0 w-full items-center gap-2 overflow-hidden">
            {triggerLabel}
            <ChevronDownIcon aria-hidden="true" className="size-3 shrink-0 opacity-60" />
          </span>
        ) : (
          <>
            <span>{triggerLabel}</span>
            <ChevronDownIcon aria-hidden="true" className="size-3 opacity-60" />
          </>
        )}
      </MenuTrigger>
      <MenuPopup align="start">
        <TraitsMenuContent
          provider={provider}
          {...(instanceId ? { instanceId } : {})}
          models={models}
          model={model}
          prompt={prompt}
          onPromptChange={onPromptChange}
          modelOptions={modelOptions}
          allowPromptInjectedEffort={allowPromptInjectedEffort}
          {...persistence}
        />
      </MenuPopup>
    </Menu>
  );
});
