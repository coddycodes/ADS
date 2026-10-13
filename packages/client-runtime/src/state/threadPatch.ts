import { OrchestrationThread, type OrchestrationThreadPatch } from "@bibcode/contracts";
import * as Schema from "effect/Schema";
import * as Struct from "effect/Struct";

// The thread with every field optional: one decode of a patch's changed values
// gives exactly the full thread decode's result for those fields, optional and
// nullable handling included, without decoding the unchanged rest. Fields a
// newer server adds are dropped, as a full decode drops them.
const decodeChanges = Schema.decodeUnknownSync(
  Schema.toCodecJson(OrchestrationThread.mapFields(Struct.map(Schema.optionalKey))),
) as (value: unknown) => Record<string, unknown>;
const knownFields = new Set(Object.keys(OrchestrationThread.fields));

/**
 * Applies a server patch to the thread it was computed against. Arrays take
 * their new length and the decoded entries that changed; other changed fields
 * are decoded and replaced, and `unset` fields are removed.
 */
export function applyThreadPatch(
  thread: OrchestrationThread,
  patch: OrchestrationThreadPatch,
): OrchestrationThread {
  const changes = decodeChanges({
    ...patch.set,
    ...Object.fromEntries(
      Object.entries(patch.arrays).map(([key, array]) => [
        key,
        array.changes.map(([, item]) => item),
      ]),
    ),
  });
  const next: Record<string, unknown> = { ...thread };
  for (const key of patch.unset) delete next[key];
  for (const key of Object.keys(patch.set)) {
    if (!knownFields.has(key)) continue;
    // A field the decode leaves out was sent as null for an optional value.
    if (key in changes) next[key] = changes[key];
    else delete next[key];
  }
  for (const [key, array] of Object.entries(patch.arrays)) {
    if (!knownFields.has(key)) continue;
    const current = Array.isArray(next[key]) ? (next[key] as ReadonlyArray<unknown>) : [];
    const items = (changes[key] as ReadonlyArray<unknown> | undefined) ?? [];
    const updated = current.slice(0, array.length);
    array.changes.forEach(([index], position) => {
      updated[index] = items[position];
    });
    next[key] = updated;
  }
  return next as OrchestrationThread;
}
