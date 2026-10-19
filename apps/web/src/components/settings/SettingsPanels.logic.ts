import type {
  ProviderDriverKind,
  ProviderInstanceConfig,
  ProviderInstanceId,
  ProviderSessionDefault,
  ServerSettings,
  UnifiedSettings,
} from "@bibcode/contracts";
import { DEFAULT_UNIFIED_SETTINGS } from "@bibcode/contracts/settings";

type ProviderSessionDefaultsMap = ServerSettings["providerSessionDefaults"];

export interface ProviderSessionDefaultsDraft {
  readonly submit: (
    driver: ProviderDriverKind,
    next: ProviderSessionDefault,
  ) => ProviderSessionDefaultsSubmission;
  readonly reconcile: (authoritative: ProviderSessionDefaultsMap) => ProviderSessionDefaultsMap;
  readonly reject: (revision: ProviderSessionDefaultsRevision) => ProviderSessionDefaultsMap;
}

declare const providerSessionDefaultsRevisionBrand: unique symbol;

export type ProviderSessionDefaultsRevision = Readonly<{
  [providerSessionDefaultsRevisionBrand]: true;
}>;

export interface ProviderSessionDefaultsSubmission {
  readonly revision: ProviderSessionDefaultsRevision;
  readonly defaults: ProviderSessionDefaultsMap;
}

interface PendingSettingsMapSubmission<K extends string, V, M> {
  readonly revision: object;
  readonly key: K;
  readonly next: V;
  snapshot: M;
  acknowledged: boolean;
}

function sameProviderSessionDefault(
  left: ProviderSessionDefault | undefined,
  right: ProviderSessionDefault | undefined,
): boolean {
  if (left === right) return true;
  if (!left || !right || left.model !== right.model) return false;
  const leftOptions = left.options ?? [];
  const rightOptions = right.options ?? [];
  return (
    leftOptions.length === rightOptions.length &&
    leftOptions.every(
      (selection, index) =>
        selection.id === rightOptions[index]?.id && selection.value === rightOptions[index]?.value,
    )
  );
}

function cloneProviderSessionDefault(value: ProviderSessionDefault): ProviderSessionDefault {
  return {
    ...value,
    ...(value.options ? { options: value.options.map((selection) => ({ ...selection })) } : {}),
  };
}

export function createSettingsMapDraft<K extends string, V, M extends Readonly<Record<string, V>>>(
  initial: M,
  cloneValue: (value: V) => V,
  sameValue: (left: V | undefined, right: V | undefined) => boolean,
) {
  const cloneMap = (input: M): M =>
    Object.fromEntries(Object.entries(input).map(([key, value]) => [key, cloneValue(value)])) as M;
  const matchesSnapshot = (authoritative: M, submitted: M): boolean =>
    Object.entries(submitted).every(([key, value]) => sameValue(authoritative[key], value));
  const submissions: Array<PendingSettingsMapSubmission<K, V, M>> = [];
  let latestAuthoritative = cloneMap(initial);
  let current = cloneMap(latestAuthoritative);

  const rebuildPendingState = (): M => {
    current = cloneMap(latestAuthoritative);
    let hasPending = false;
    for (const submission of submissions) {
      if (submission.acknowledged) continue;
      hasPending = true;
      current = {
        ...current,
        [submission.key]: cloneValue(submission.next),
      } as M;
      submission.snapshot = cloneMap(current);
    }
    if (!hasPending) {
      submissions.length = 0;
    }
    return cloneMap(current);
  };

  return {
    submit(key: K, next: V) {
      const clonedNext = cloneValue(next);
      current = { ...current, [key]: clonedNext } as M;
      const revision = Object.freeze({});
      submissions.push({
        revision,
        key,
        next: clonedNext,
        snapshot: cloneMap(current),
        acknowledged: false,
      });
      return {
        revision,
        map: cloneMap(current),
      };
    },
    reconcile(authoritative: M) {
      const authoritativeSnapshot = cloneMap(authoritative);
      let acknowledgedIndex = -1;
      for (let index = submissions.length - 1; index >= 0; index -= 1) {
        const submission = submissions[index];
        if (submission && matchesSnapshot(authoritativeSnapshot, submission.snapshot)) {
          acknowledgedIndex = index;
          break;
        }
      }

      if (acknowledgedIndex === -1) {
        submissions.length = 0;
        latestAuthoritative = authoritativeSnapshot;
        current = cloneMap(authoritativeSnapshot);
        return cloneMap(current);
      }

      latestAuthoritative = authoritativeSnapshot;
      for (let index = 0; index <= acknowledgedIndex; index += 1) {
        const submission = submissions[index];
        if (submission) submission.acknowledged = true;
      }
      return rebuildPendingState();
    },
    reject(revision: object) {
      const rejectedIndex = submissions.findIndex((submission) => submission.revision === revision);
      if (rejectedIndex === -1) {
        return cloneMap(current);
      }
      submissions.splice(rejectedIndex, 1);
      return rebuildPendingState();
    },
  };
}

export function createProviderSessionDefaultsDraft(
  initial: ProviderSessionDefaultsMap,
): ProviderSessionDefaultsDraft {
  const draft = createSettingsMapDraft(
    initial,
    cloneProviderSessionDefault,
    sameProviderSessionDefault,
  );
  return {
    submit(driver, next) {
      const submission = draft.submit(driver, next);
      return {
        revision: submission.revision as ProviderSessionDefaultsRevision,
        defaults: submission.map,
      };
    },
    reconcile: draft.reconcile,
    reject: draft.reject,
  };
}

export function buildProviderInstanceUpdatePatch(input: {
  readonly settings: Pick<ServerSettings, "providers" | "providerInstances">;
  readonly instanceId: ProviderInstanceId;
  readonly instance: ProviderInstanceConfig;
  readonly driver: ProviderDriverKind;
  readonly isDefault: boolean;
  readonly textGenerationModelSelection?:
    | ServerSettings["textGenerationModelSelection"]
    | undefined;
}): Partial<UnifiedSettings> {
  type LegacyProviderSettings = ServerSettings["providers"][keyof ServerSettings["providers"]];
  const legacyProviderDefaults = DEFAULT_UNIFIED_SETTINGS.providers as Record<
    string,
    LegacyProviderSettings | undefined
  >;
  const legacyProviderDefault = input.isDefault ? legacyProviderDefaults[input.driver] : undefined;
  return {
    ...(legacyProviderDefault !== undefined
      ? {
          providers: {
            ...input.settings.providers,
            [input.driver]: legacyProviderDefault,
          } as ServerSettings["providers"],
        }
      : {}),
    providerInstances: {
      ...input.settings.providerInstances,
      [input.instanceId]: input.instance,
    },
    ...(input.textGenerationModelSelection !== undefined
      ? { textGenerationModelSelection: input.textGenerationModelSelection }
      : {}),
  };
}
