/**
 * Instance-aware view over the wire `ServerProvider[]`.
 *
 * The wire carries one `ServerProvider` per *configured instance* — the
 * default built-in codex instance, a user-authored `codex_personal`, an
 * unavailable shadow for a fork driver, etc. Legacy UI code collapsed these
 * into a single bucket per built-in driver via `.find((p) => p.driver === kind)`,
 * which silently dropped every custom instance after the first. This module
 * replaces that pattern with `ProviderInstanceEntry[]`, keyed on
 * `ProviderInstanceId`, so the model picker, settings list, and composer
 * can treat built-in and custom instances uniformly.
 *
 * @module providerInstances
 */
import {
  defaultInstanceIdForDriver,
  PROVIDER_DISPLAY_NAMES,
  type ProviderDriverKind,
  type ProviderInstanceId,
  type ServerProvider,
  type ServerProviderModel,
  type ServerSettings,
  type ServerProviderState,
} from "@coddy/contracts";

import { formatProviderDriverKindLabel } from "./providerModels";

/**
 * UI-facing projection of one configured provider instance. Carries the
 * snapshot verbatim for callers that need server-side fields we don't
 * hoist here, plus the precomputed `instanceId` / `driverKind` /
 * `displayName` used by every picker and settings view.
 */
export interface ProviderInstanceEntry {
  readonly instanceId: ProviderInstanceId;
  readonly driverKind: ProviderDriverKind;
  readonly displayName: string;
  readonly accentColor?: string | undefined;
  readonly continuationGroupKey?: string | undefined;
  readonly enabled: boolean;
  readonly installed: boolean;
  readonly status: ServerProviderState;
  /**
   * True when this entry is the default instance for its driver kind —
   * i.e. its instance id equals `defaultInstanceIdForDriver(driverKind)`.
   * The settings panel and picker sort defaults before customs.
   */
  readonly isDefault: boolean;
  /** True when `availability === "unavailable"` is absent or "available". */
  readonly isAvailable: boolean;
  readonly snapshot: ServerProvider;
  readonly models: ReadonlyArray<ServerProviderModel>;
}

/**
 * Whether an instance can currently contribute models to an interactive picker.
 *
 * Disabling an instance updates `enabled` independently, while its previous
 * `ready` probe status can remain in the streamed snapshot until reconciliation.
 */
export function isProviderInstancePickerReady(entry: ProviderInstanceEntry): boolean {
  return entry.enabled && entry.isAvailable && entry.status === "ready";
}

/** Picker rails contain configured, enabled instances only. */
export function isProviderInstancePickerVisible(entry: ProviderInstanceEntry): boolean {
  return entry.enabled;
}

export function providerDriverLabel(driverKind: ProviderDriverKind): string {
  return PROVIDER_DISPLAY_NAMES[driverKind] ?? formatProviderDriverKindLabel(driverKind);
}

export function normalizeProviderAccentColor(value: string | undefined): string | undefined {
  const trimmed = value?.trim();
  if (!trimmed) return undefined;
  return /^#[0-9a-fA-F]{6}$/u.test(trimmed) ? trimmed : undefined;
}

/**
 * The server stamps its instance label in every snapshot. Older servers may
 * omit it; use the driver's name, matching those servers' messages.
 */
function resolveInstanceDisplayName(snapshot: ServerProvider): string {
  return snapshot.displayName?.trim() || providerDriverLabel(snapshot.driver);
}

/**
 * Project the wire `ServerProvider[]` into instance entries, one per
 * configured instance. Preserves the server's ordering (which sources
 * from `deriveProviderInstanceConfigMap` — explicit `providerInstances.*`
 * first, synthesized defaults after) so callers that want "default first"
 * should sort with `sortProviderInstanceEntries` below.
 */
export function deriveProviderInstanceEntries(
  providers: ReadonlyArray<ServerProvider>,
): ReadonlyArray<ProviderInstanceEntry> {
  return providers.map((snapshot) => {
    const instanceId = snapshot.instanceId;
    const driverKind = snapshot.driver;
    const defaultId = defaultInstanceIdForDriver(driverKind);
    const isDefault = instanceId === defaultId;
    const displayName = resolveInstanceDisplayName(snapshot);
    return {
      instanceId,
      driverKind,
      displayName,
      accentColor: normalizeProviderAccentColor(snapshot.accentColor),
      continuationGroupKey: snapshot.continuation?.groupKey,
      enabled: snapshot.enabled,
      installed: snapshot.installed,
      status: snapshot.status,
      isDefault,
      isAvailable: snapshot.availability !== "unavailable",
      snapshot,
      models: snapshot.models,
    } satisfies ProviderInstanceEntry;
  });
}

/**
 * Overlay the current settings configuration onto streamed provider snapshots.
 * Provider probes can briefly retain their previous `enabled` value after a
 * settings write, so picker visibility must follow settings rather than waiting
 * for probe reconciliation.
 *
 * Non-default instances only exist through `providerInstances`; if one is
 * absent there, its streamed snapshot is stale (for example immediately after
 * deletion) and is treated as disabled.
 */
export function applyProviderInstanceSettings(
  entries: ReadonlyArray<ProviderInstanceEntry>,
  settings: Pick<ServerSettings, "providerInstances" | "providers">,
): ReadonlyArray<ProviderInstanceEntry> {
  const legacyProviders = settings.providers as Readonly<
    Record<string, { readonly enabled?: boolean } | undefined>
  >;

  return entries.map((entry) => {
    const explicitInstance = settings.providerInstances?.[entry.instanceId];
    const enabled = explicitInstance
      ? (explicitInstance.enabled ?? true)
      : entry.isDefault
        ? (legacyProviders[entry.driverKind]?.enabled ?? entry.enabled)
        : false;
    const configuredDisplayName = explicitInstance?.displayName?.trim();
    const displayName = configuredDisplayName || entry.displayName;
    if (enabled === entry.enabled && displayName === entry.displayName) {
      return entry;
    }
    return { ...entry, enabled, displayName };
  });
}

/**
 * Sort instance entries so the default instance of each driver kind appears
 * before any custom instances of the same kind. Within a kind, custom
 * instances keep their settings-author order (which is how the server
 * emits them). Stable across kinds: entries retain the server's
 * cross-driver ordering.
 */
export function sortProviderInstanceEntries(
  entries: ReadonlyArray<ProviderInstanceEntry>,
): ReadonlyArray<ProviderInstanceEntry> {
  // Group by driver kind preserving first-appearance order, then emit
  // default-first within each kind. Using a Map keeps the "first-seen"
  // semantics for kinds whose default instance is absent (unusual but
  // possible during the migration).
  const byKind = new Map<ProviderDriverKind, ProviderInstanceEntry[]>();
  for (const entry of entries) {
    const bucket = byKind.get(entry.driverKind);
    if (bucket) {
      bucket.push(entry);
    } else {
      byKind.set(entry.driverKind, [entry]);
    }
  }
  const sorted: ProviderInstanceEntry[] = [];
  for (const bucket of byKind.values()) {
    const defaults = bucket.filter((entry) => entry.isDefault);
    const customs = bucket.filter((entry) => !entry.isDefault);
    sorted.push(...defaults, ...customs);
  }
  return sorted;
}

/**
 * Look up a single instance entry by exact `instanceId`. Missing snapshots
 * are not inferred from driver kind in UI routing code.
 */
export function getProviderInstanceEntry(
  providers: ReadonlyArray<ServerProvider>,
  instanceId: ProviderInstanceId,
): ProviderInstanceEntry | undefined {
  return deriveProviderInstanceEntries(providers).find((entry) => entry.instanceId === instanceId);
}

/**
 * Model list for a specific instance. Returns `[]` when the instance isn't
 * present so callers don't have to thread optionality through render code.
 */
export function getProviderInstanceModels(
  providers: ReadonlyArray<ServerProvider>,
  instanceId: ProviderInstanceId,
): ReadonlyArray<ServerProviderModel> {
  return getProviderInstanceEntry(providers, instanceId)?.models ?? [];
}

/**
 * Resolve the routing key for a selection that may reference an instance
 * id that no longer exists (e.g. a persisted thread selection after the
 * user deleted the custom instance). Returns the first enabled instance
 * as a fallback so downstream code can still send a turn.
 */
export function resolveSelectableProviderInstance(
  providers: ReadonlyArray<ServerProvider>,
  instanceId: ProviderInstanceId | undefined,
): ProviderInstanceId | undefined {
  if (instanceId === undefined) {
    return deriveProviderInstanceEntries(providers).find(
      (entry) => entry.enabled && entry.isAvailable,
    )?.instanceId;
  }
  const entries = deriveProviderInstanceEntries(providers);
  const requested = entries.find((entry) => entry.instanceId === instanceId);
  if (requested && requested.enabled && requested.isAvailable) {
    return instanceId;
  }
  return entries.find((entry) => entry.enabled && entry.isAvailable)?.instanceId;
}

/**
 * Resolve an open model-selection routing key back to a driver kind.
 * Custom instance ids such as `claude_openrouter` are not themselves
 * driver-kind slugs, but the composer still needs the owning driver kind
 * for capabilities, options, icons, and turn dispatch metadata.
 */
export function resolveProviderDriverKindForInstanceSelection(
  entries: ReadonlyArray<ProviderInstanceEntry>,
  providers: ReadonlyArray<ServerProvider>,
  selection: ProviderInstanceId | ProviderDriverKind | null | undefined,
): ProviderDriverKind | undefined {
  const matchedEntry = entries.find((entry) => entry.instanceId === selection);
  if (matchedEntry) {
    return matchedEntry.driverKind;
  }
  return undefined;
}
