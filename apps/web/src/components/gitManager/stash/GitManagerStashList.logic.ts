import type {
  GitManagerBlockedReason,
  GitManagerChangedFile,
  GitManagerStashEntry,
} from "@coddy/contracts";

export interface GitManagerStashRow {
  readonly index: number;
  readonly sha: string;
  readonly message: string;
  readonly committedAtMs: number;
  readonly parents: ReadonlyArray<string>;
  readonly files: ReadonlyArray<GitManagerChangedFile>;
  readonly blocked: GitManagerBlockedReason | null;
}

export function buildStashRows(
  entries: ReadonlyArray<GitManagerStashEntry>,
  blockedReasons: ReadonlyArray<GitManagerBlockedReason>,
): ReadonlyArray<GitManagerStashRow> {
  const blocked = blockedReasons[0] ?? null;
  return entries.map((entry) => ({ ...entry, blocked }));
}

export function resolveStashIndex(
  entries: ReadonlyArray<GitManagerStashEntry>,
  sha: string,
): number | null {
  return entries.find((entry) => entry.sha === sha)?.index ?? null;
}

interface StashActionAvailability {
  readonly enabled: boolean;
  readonly reason: string | null;
}

export interface StashActionState {
  readonly apply: StashActionAvailability;
  readonly pop: StashActionAvailability;
  readonly drop: StashActionAvailability;
}

export function resolveStashActionState(
  row: GitManagerStashRow,
  _input: { readonly operationInFlight: boolean },
): StashActionState {
  const availability = {
    enabled: row.blocked === null,
    reason: row.blocked?.message ?? null,
  };
  return {
    apply: availability,
    pop: availability,
    drop: availability,
  };
}

export interface StashDiscardDialogCopy {
  readonly title: string;
  readonly body: string;
  readonly confirmLabel: string;
  readonly destructive: true;
}

export function resolveStashDiscardDialogCopy(row: GitManagerStashRow): StashDiscardDialogCopy {
  const selector = `stash@{${row.index}}`;
  return {
    title: `Drop ${selector}?`,
    body: `Drop ${selector} (${row.message})? This entry cannot be recovered.`,
    confirmLabel: "Drop Stash",
    destructive: true,
  };
}
