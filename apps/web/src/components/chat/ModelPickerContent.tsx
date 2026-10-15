import {
  type ProviderInstanceId,
  type ProviderDriverKind,
  type ResolvedKeybindingsConfig,
} from "@bibcode/contracts";
import { resolveSelectableModel } from "@bibcode/shared/model";
import { LegendList, type LegendListRef } from "@legendapp/list/react";
import { memo, useMemo, useState, useCallback, useEffect, useLayoutEffect, useRef } from "react";
import { SearchIcon } from "lucide-react";
import { ModelListRow } from "./ModelListRow";
import { isModelPickerNewModel } from "./modelPickerModelHighlights";
import { buildModelPickerSearchText, scoreModelPickerSearch } from "./modelPickerSearch";
import { Combobox, ComboboxEmpty, ComboboxInput, ComboboxListVirtualized } from "../ui/combobox";
import { ModelEsque } from "./providerIconUtils";
import {
  modelPickerJumpCommandForIndex,
  modelPickerJumpIndexFromCommand,
  resolveShortcutCommand,
  shortcutLabelForCommand,
} from "../../keybindings";
import { useClientSettings, useUpdateClientSettings } from "~/hooks/useSettings";
import { cn } from "~/lib/utils";
import { TooltipProvider } from "../ui/tooltip";
import { isProviderInstancePickerReady, type ProviderInstanceEntry } from "../../providerInstances";
import { providerModelKey, sortProviderModelItems } from "../../modelOrdering";

type ModelPickerItem = {
  slug: string;
  name: string;
  shortName?: string;
  subProvider?: string;
  instanceId: ProviderInstanceId;
  driverKind: ProviderDriverKind;
  instanceDisplayName: string;
  instanceAccentColor?: string | undefined;
  continuationGroupKey?: string | undefined;
};

const EMPTY_MODEL_JUMP_LABELS = new Map<string, string>();

// Split a `${instanceId}:${slug}` combobox key back into its pieces. Slugs
// can contain colons (e.g. some vendor model ids), so we only split on the
// first colon — anything after that is the slug.
function splitInstanceModelKey(key: string): { instanceId: ProviderInstanceId; slug: string } {
  const colonIndex = key.indexOf(":");
  if (colonIndex === -1) {
    return { instanceId: key as ProviderInstanceId, slug: "" };
  }
  return {
    instanceId: key.slice(0, colonIndex) as ProviderInstanceId,
    slug: key.slice(colonIndex + 1),
  };
}

export const ModelPickerContent = memo(function ModelPickerContent(props: {
  /** The instance currently selected in the composer (combobox "value"). */
  activeInstanceId: ProviderInstanceId;
  model: string;
  /** Restrict the picker to the provider instance that owns the chat panel. */
  lockToActiveInstance?: boolean;
  /**
   * When set, the picker is locked to the given driver kind — typically
   * because the user is editing a previously-sent message and can't change
   * which driver served the turn. Multiple instances of the same kind
   * remain selectable (e.g. locked to `codex` still lets the user switch
   * between the default Codex and a custom Codex Personal).
   */
  lockedProvider: ProviderDriverKind | null;
  lockedContinuationGroupKey?: string | null;
  /**
   * All configured provider instances in display order. Used to order
   * models and resolve provider display names.
   */
  instanceEntries: ReadonlyArray<ProviderInstanceEntry>;
  keybindings?: ResolvedKeybindingsConfig;
  /**
   * Model options per instance. Keyed by `ProviderInstanceId` so the
   * default Codex instance and any custom Codex instances each have their
   * own list (custom instances typically start with the same built-in
   * model set but are free to diverge via customModels).
   */
  modelOptionsByInstance: ReadonlyMap<ProviderInstanceId, ReadonlyArray<ModelEsque>>;
  terminalOpen: boolean;
  onRequestClose?: () => void;
  getModelDisabledReason?: (instanceId: ProviderInstanceId, model: string) => string | null;
  onInstanceModelChange: (instanceId: ProviderInstanceId, model: string) => void;
}) {
  const {
    keybindings: providedKeybindings,
    modelOptionsByInstance,
    instanceEntries,
    getModelDisabledReason,
    onInstanceModelChange,
  } = props;
  const [searchQuery, setSearchQuery] = useState("");
  const [showTopScrollFade, setShowTopScrollFade] = useState(false);
  const [showBottomScrollFade, setShowBottomScrollFade] = useState(false);
  const [scrollFadeModelKeys, setScrollFadeModelKeys] = useState<readonly string[] | null>(null);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const modelListRef = useRef<LegendListRef | null>(null);
  const highlightedModelKeyRef = useRef<string | null>(null);
  const favorites = useClientSettings((s) => s.favorites ?? []);
  const keybindings = useMemo<ResolvedKeybindingsConfig>(
    () => providedKeybindings ?? [],
    [providedKeybindings],
  );
  const updateSettings = useUpdateClientSettings();

  const focusSearchInput = useCallback(() => {
    searchInputRef.current?.focus({ preventScroll: true });
  }, []);

  useLayoutEffect(() => {
    focusSearchInput();
    const frame = window.requestAnimationFrame(() => {
      focusSearchInput();
    });
    const timeout = window.setTimeout(() => {
      focusSearchInput();
    }, 0);
    return () => {
      window.cancelAnimationFrame(frame);
      window.clearTimeout(timeout);
    };
  }, [focusSearchInput]);

  // Create a Set for efficient lookup. Favorites are keyed by
  // `${instanceId}:${slug}`; the storage schema widened from ProviderDriverKind
  // to ProviderInstanceId so pre-migration favorites keyed by driver slugs
  // (e.g. `"codex:gpt-5"`) still resolve — the default instance id equals
  // the driver slug.
  const favoritesSet = useMemo(() => {
    return new Set(favorites.map((fav) => providerModelKey(fav.provider, fav.model)));
  }, [favorites]);

  /**
   * Lookup table keyed by `instanceId`. Used for display name + driver
   * kind enrichment and for `ready`/enabled filtering before flattening
   * models into the search list.
   */
  const entryByInstanceId = useMemo(
    () => new Map(instanceEntries.map((entry) => [entry.instanceId, entry])),
    [instanceEntries],
  );
  const matchesLockedProvider = useCallback(
    (entry: Pick<ProviderInstanceEntry, "driverKind" | "continuationGroupKey">): boolean => {
      if (props.lockedProvider === null) return true;
      if (entry.driverKind !== props.lockedProvider) return false;
      if (!props.lockedContinuationGroupKey) return true;
      return entry.continuationGroupKey === props.lockedContinuationGroupKey;
    },
    [props.lockedContinuationGroupKey, props.lockedProvider],
  );

  const readyInstanceSet = useMemo(() => {
    const ready = new Set<ProviderInstanceId>();
    for (const entry of instanceEntries) {
      if (isProviderInstancePickerReady(entry)) {
        ready.add(entry.instanceId);
      }
    }
    return ready;
  }, [instanceEntries]);

  // Flatten models into a searchable array. One pass over the
  // instance-keyed map; each model carries its instance id + driver kind
  // so the list row can render the right icon and display name without
  // another lookup.
  const flatModels = useMemo(() => {
    const out: ModelPickerItem[] = [];
    for (const [instanceId, models] of modelOptionsByInstance) {
      if (props.lockToActiveInstance && instanceId !== props.activeInstanceId) {
        continue;
      }
      const entry = entryByInstanceId.get(instanceId);
      if (!entry) {
        // Instance disappeared between renders (configuration change). Skip
        // its models — stale options shouldn't appear in the picker.
        continue;
      }
      if (!readyInstanceSet.has(instanceId)) {
        continue;
      }
      for (const model of models) {
        out.push({
          slug: model.slug,
          name: model.name,
          ...(model.shortName ? { shortName: model.shortName } : {}),
          ...(model.subProvider ? { subProvider: model.subProvider } : {}),
          instanceId,
          driverKind: entry.driverKind,
          instanceDisplayName: entry.displayName,
          ...(entry.accentColor ? { instanceAccentColor: entry.accentColor } : {}),
          ...(entry.continuationGroupKey
            ? { continuationGroupKey: entry.continuationGroupKey }
            : {}),
        });
      }
    }
    return out;
  }, [
    modelOptionsByInstance,
    entryByInstanceId,
    readyInstanceSet,
    props.activeInstanceId,
    props.lockToActiveInstance,
  ]);

  const isLocked = props.lockedProvider !== null;
  const instanceOrder = useMemo(
    () => instanceEntries.map((entry) => entry.instanceId),
    [instanceEntries],
  );

  // Filter models based on search query and thread constraints.
  const filteredModels = useMemo(() => {
    let result = flatModels;

    // Apply tokenized fuzzy search across the combined provider/model search fields.
    if (searchQuery.trim()) {
      const rankedMatches = result
        .map((model) => ({
          model,
          score: scoreModelPickerSearch(
            {
              name: model.name,
              ...(model.shortName ? { shortName: model.shortName } : {}),
              ...(model.subProvider ? { subProvider: model.subProvider } : {}),
              driverKind: model.driverKind,
              providerDisplayName: model.instanceDisplayName,
              isFavorite: favoritesSet.has(providerModelKey(model.instanceId, model.slug)),
            },
            searchQuery,
          ),
          isFavorite: favoritesSet.has(providerModelKey(model.instanceId, model.slug)),
          tieBreaker: buildModelPickerSearchText({
            name: model.name,
            ...(model.shortName ? { shortName: model.shortName } : {}),
            ...(model.subProvider ? { subProvider: model.subProvider } : {}),
            driverKind: model.driverKind,
            providerDisplayName: model.instanceDisplayName,
          }),
        }))
        .filter(
          (
            rankedModel,
          ): rankedModel is {
            model: ModelPickerItem;
            score: number;
            isFavorite: boolean;
            tieBreaker: string;
          } => rankedModel.score !== null,
        );

      // When searching, respect the locked provider while ranking matches
      // across every otherwise-selectable provider instance.
      if (props.lockedProvider !== null) {
        const lockedProviderMatches: Array<(typeof rankedMatches)[number]> = [];
        for (const rankedModel of rankedMatches) {
          if (matchesLockedProvider(rankedModel.model)) {
            lockedProviderMatches.push(rankedModel);
          }
        }
        return lockedProviderMatches
          .toSorted((a, b) => {
            const scoreDelta = a.score - b.score;
            if (scoreDelta !== 0) {
              return scoreDelta;
            }
            if (a.isFavorite !== b.isFavorite) {
              return a.isFavorite ? -1 : 1;
            }
            return a.tieBreaker.localeCompare(b.tieBreaker);
          })
          .map((rankedModel) => rankedModel.model);
      }

      return rankedMatches
        .toSorted((a, b) => {
          const scoreDelta = a.score - b.score;
          if (scoreDelta !== 0) {
            return scoreDelta;
          }
          if (a.isFavorite !== b.isFavorite) {
            return a.isFavorite ? -1 : 1;
          }
          return a.tieBreaker.localeCompare(b.tieBreaker);
        })
        .map((rankedModel) => rankedModel.model);
    }

    if (props.lockedProvider !== null) {
      result = result.filter((m) => matchesLockedProvider(m));
    }

    return sortProviderModelItems(result, {
      favoriteModelKeys: favoritesSet,
      groupFavorites: true,
      instanceOrder,
    });
  }, [
    favoritesSet,
    flatModels,
    instanceOrder,
    matchesLockedProvider,
    props.lockedProvider,
    searchQuery,
  ]);

  const handleModelSelect = useCallback(
    (modelSlug: string, instanceId: ProviderInstanceId) => {
      if (props.lockToActiveInstance && instanceId !== props.activeInstanceId) {
        return;
      }
      if (getModelDisabledReason?.(instanceId, modelSlug)) {
        return;
      }
      const options = modelOptionsByInstance.get(instanceId);
      if (!options) {
        return;
      }
      const entry = entryByInstanceId.get(instanceId);
      if (!entry) {
        return;
      }
      // `resolveSelectableModel` uses the driver kind for normalization
      // (slug casing etc.). Custom instances share their driver's
      // normalization rules, so pass the driver kind here.
      const resolvedModel = resolveSelectableModel(entry.driverKind, modelSlug, options);
      if (resolvedModel) {
        onInstanceModelChange(instanceId, resolvedModel);
      }
    },
    [
      entryByInstanceId,
      getModelDisabledReason,
      modelOptionsByInstance,
      onInstanceModelChange,
      props.activeInstanceId,
      props.lockToActiveInstance,
    ],
  );

  const toggleFavorite = useCallback(
    (instanceId: ProviderInstanceId, model: string) => {
      const newFavorites = [...favorites];
      const index = newFavorites.findIndex((f) => f.provider === instanceId && f.model === model);
      if (index >= 0) {
        newFavorites.splice(index, 1);
      } else {
        newFavorites.push({ provider: instanceId, model });
      }
      updateSettings({ favorites: newFavorites });
    },
    [favorites, updateSettings],
  );

  const modelJumpCommandByKey = useMemo(() => {
    const mapping = new Map<
      string,
      NonNullable<ReturnType<typeof modelPickerJumpCommandForIndex>>
    >();
    let selectableModelIndex = 0;
    for (const model of filteredModels) {
      if (getModelDisabledReason?.(model.instanceId, model.slug)) {
        continue;
      }
      const jumpCommand = modelPickerJumpCommandForIndex(selectableModelIndex);
      if (!jumpCommand) {
        return mapping;
      }
      mapping.set(`${model.instanceId}:${model.slug}`, jumpCommand);
      selectableModelIndex += 1;
    }
    return mapping;
  }, [filteredModels, getModelDisabledReason]);
  const modelJumpModelKeys = useMemo(
    () => [...modelJumpCommandByKey.keys()],
    [modelJumpCommandByKey],
  );
  const allModelKeys = useMemo(
    (): string[] => flatModels.map((model) => `${model.instanceId}:${model.slug}`),
    [flatModels],
  );
  const filteredModelKeys = useMemo(
    (): string[] => filteredModels.map((model) => `${model.instanceId}:${model.slug}`),
    [filteredModels],
  );
  const filteredModelByKey = useMemo(
    (): ReadonlyMap<string, ModelPickerItem> =>
      new Map(filteredModels.map((model) => [`${model.instanceId}:${model.slug}`, model] as const)),
    [filteredModels],
  );
  const updateModelListScrollFades = useCallback(() => {
    const scrollElement = modelListRef.current?.getScrollableNode();
    if (!(scrollElement instanceof HTMLElement)) {
      return;
    }
    const maxScrollOffset = Math.max(0, scrollElement.scrollHeight - scrollElement.clientHeight);
    setShowTopScrollFade(scrollElement.scrollTop > 1);
    setShowBottomScrollFade(maxScrollOffset - scrollElement.scrollTop > 1);
  }, []);
  const modelJumpShortcutContext = useMemo(
    () =>
      ({
        terminalFocus: false,
        terminalOpen: props.terminalOpen,
        modelPickerOpen: true,
      }) as const,
    [props.terminalOpen],
  );
  const modelJumpLabelByKey = useMemo((): ReadonlyMap<string, string> => {
    if (modelJumpCommandByKey.size === 0) {
      return EMPTY_MODEL_JUMP_LABELS;
    }
    const shortcutLabelOptions = {
      platform: navigator.platform,
      context: modelJumpShortcutContext,
    };
    const mapping = new Map<string, string>();
    for (const [modelKey, command] of modelJumpCommandByKey) {
      const label = shortcutLabelForCommand(keybindings, command, shortcutLabelOptions);
      if (label) {
        mapping.set(modelKey, label);
      }
    }
    return mapping.size > 0 ? mapping : EMPTY_MODEL_JUMP_LABELS;
  }, [keybindings, modelJumpCommandByKey, modelJumpShortcutContext]);

  useEffect(() => {
    const onWindowKeyDown = (event: globalThis.KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat) {
        return;
      }

      const command = resolveShortcutCommand(event, keybindings, {
        platform: navigator.platform,
        context: modelJumpShortcutContext,
      });
      const jumpIndex = modelPickerJumpIndexFromCommand(command ?? "");
      if (jumpIndex === null) {
        return;
      }

      const targetModelKey = modelJumpModelKeys[jumpIndex];
      if (!targetModelKey) {
        return;
      }
      const { instanceId, slug } = splitInstanceModelKey(targetModelKey);
      event.preventDefault();
      event.stopPropagation();
      handleModelSelect(slug, instanceId);
    };

    window.addEventListener("keydown", onWindowKeyDown, true);

    return () => {
      window.removeEventListener("keydown", onWindowKeyDown, true);
    };
  }, [handleModelSelect, keybindings, modelJumpModelKeys, modelJumpShortcutContext]);

  // Reset the fades for a new result list before paint; the frames below measure the real ones.
  if (scrollFadeModelKeys !== filteredModelKeys) {
    setScrollFadeModelKeys(filteredModelKeys);
    setShowTopScrollFade(false);
    setShowBottomScrollFade(filteredModelKeys.length > 5);
  }

  useLayoutEffect(() => {
    // An empty list cannot scroll, and the reset above already cleared both fades.
    if (filteredModelKeys.length === 0) {
      return;
    }
    let nestedFrame = 0;
    const frame = window.requestAnimationFrame(() => {
      updateModelListScrollFades();
      nestedFrame = window.requestAnimationFrame(updateModelListScrollFades);
    });
    return () => {
      window.cancelAnimationFrame(frame);
      window.cancelAnimationFrame(nestedFrame);
    };
  }, [filteredModelKeys, updateModelListScrollFades]);

  return (
    <TooltipProvider delay={0}>
      <div
        className="relative flex h-screen max-h-96 w-screen max-w-100 flex-row overflow-hidden rounded-lg border bg-popover not-dark:bg-clip-padding text-popover-foreground shadow-lg/5 before:pointer-events-none before:absolute before:inset-0 before:rounded-[calc(var(--radius-lg)-1px)] before:shadow-[0_1px_--theme(--color-black/4%)] dark:before:shadow-[0_-1px_--theme(--color-white/6%)]"
        data-model-picker-content="true"
      >
        {/* Main content area */}
        <Combobox
          inline
          items={allModelKeys}
          filteredItems={filteredModelKeys}
          filter={null}
          autoHighlight
          open
          virtualized
          value={`${props.activeInstanceId}:${props.model}`}
          onItemHighlighted={(modelKey, eventDetails) => {
            highlightedModelKeyRef.current = typeof modelKey === "string" ? modelKey : null;
            if (eventDetails.reason === "keyboard" && eventDetails.index >= 0) {
              void modelListRef.current?.scrollIndexIntoView?.({
                index: eventDetails.index,
                animated: false,
              });
            }
          }}
          onValueChange={(modelKey) => {
            if (typeof modelKey !== "string") {
              return;
            }
            const { instanceId, slug } = splitInstanceModelKey(modelKey);
            handleModelSelect(slug, instanceId);
          }}
        >
          <div className="flex min-h-0 flex-1 flex-col overflow-hidden bg-muted/40">
            {/* Search bar */}
            <div className="px-4 pt-2.5">
              <div className="-translate-y-px border-b border-border/70 pb-2.5 transition-colors focus-within:border-ring">
                <ComboboxInput
                  ref={searchInputRef}
                  className="[&_input]:h-6.5 [&_input]:font-sans [&_input]:leading-6.5"
                  inputClassName="rounded-none bg-transparent text-sm"
                  placeholder="Search models..."
                  showTrigger={false}
                  startAddon={
                    <SearchIcon className="-translate-x-0.5 size-4 shrink-0 text-muted-foreground/55" />
                  }
                  value={searchQuery}
                  onChange={(e) => setSearchQuery(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Escape") {
                      e.preventDefault();
                      e.stopPropagation();
                      props.onRequestClose?.();
                      return;
                    }
                    if (e.key === "Enter" && highlightedModelKeyRef.current) {
                      (
                        e as typeof e & { preventBaseUIHandler?: () => void }
                      ).preventBaseUIHandler?.();
                      e.preventDefault();
                      e.stopPropagation();
                      const { instanceId, slug } = splitInstanceModelKey(
                        highlightedModelKeyRef.current,
                      );
                      handleModelSelect(slug, instanceId);
                      return;
                    }
                    e.stopPropagation();
                  }}
                  onMouseDown={(e) => e.stopPropagation()}
                  onTouchStart={(e) => e.stopPropagation()}
                  size="sm"
                  unstyled
                />
              </div>
            </div>

            {/* Model list */}
            <div className="relative min-h-0 flex-1 overflow-hidden">
              <ComboboxListVirtualized className="model-picker-list size-full min-w-0 p-0">
                <LegendList<string>
                  ref={modelListRef}
                  data={filteredModelKeys}
                  extraData={favoritesSet}
                  keyExtractor={(modelKey) => modelKey}
                  renderItem={({ item: modelKey, index }) => {
                    const model = filteredModelByKey.get(modelKey);
                    if (!model) {
                      return null;
                    }
                    const disabledReason =
                      getModelDisabledReason?.(model.instanceId, model.slug) ?? null;
                    return (
                      <ModelListRow
                        key={modelKey}
                        index={index}
                        model={model}
                        instanceId={model.instanceId}
                        driverKind={model.driverKind}
                        providerDisplayName={model.instanceDisplayName}
                        providerAccentColor={model.instanceAccentColor}
                        isFavorite={favoritesSet.has(modelKey)}
                        isSelected={modelKey === `${props.activeInstanceId}:${props.model}`}
                        showProvider
                        preferShortName={!isLocked}
                        useTriggerLabel={false}
                        showNewBadge={isModelPickerNewModel(model.driverKind, model.slug)}
                        jumpLabel={modelJumpLabelByKey.get(modelKey) ?? null}
                        disabledReason={disabledReason}
                        onToggleFavorite={() => toggleFavorite(model.instanceId, model.slug)}
                      />
                    );
                  }}
                  estimatedItemSize={48}
                  initialScrollIndex={0}
                  drawDistance={480}
                  recycleItems={false}
                  onLayout={updateModelListScrollFades}
                  onScroll={updateModelListScrollFades}
                  className={cn(
                    "scrollbar-gutter-both h-full overflow-x-hidden overscroll-y-contain py-1.5 [--fade-size:1.5rem]",
                    showTopScrollFade && "mask-t-from-[calc(100%-var(--fade-size))]",
                    showBottomScrollFade && "mask-b-from-[calc(100%-var(--fade-size))]",
                  )}
                />
              </ComboboxListVirtualized>
            </div>
            <ComboboxEmpty className="not-empty:py-6 empty:h-0 text-xs font-normal leading-snug">
              No models found
            </ComboboxEmpty>
          </div>
        </Combobox>
      </div>
    </TooltipProvider>
  );
});
