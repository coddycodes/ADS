import { useAtomRefresh, useAtomValue } from "@effect/atom-react";
import type {
  EnvironmentId,
  ProjectListEntriesResult,
  ProjectReadFileResult,
} from "@bibcode/contracts";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/unstable/reactivity";
import { useCallback, useEffect } from "react";

import { appAtomRegistry } from "~/rpc/atomRegistry";
import { projectEnvironment } from "~/state/projects";
import { executeAtomQuery, retryEnvironmentQuery } from "@bibcode/client-runtime/state/runtime";
import { formatEnvironmentQueryError, useEnvironmentQuery } from "~/state/query";

const EMPTY_PROJECT_FILE_PATH = "";
function optimisticFileAtom(environmentId: EnvironmentId, cwd: string, relativePath: string) {
  return projectEnvironment.optimisticFile({ environmentId, cwd, relativePath });
}

interface ProjectQueryState<A> {
  readonly data: A | null;
  readonly error: string | null;
  readonly isPending: boolean;
  /** Explicit user rescan/Retry: also clears an exhausted transport cut-off. */
  readonly refresh: () => void;
  /** Automatic re-read (signals, finished mutations); keeps cut-off state. */
  readonly revalidate: () => void;
}

export function getProjectEntriesQueryAtom(environmentId: EnvironmentId, cwd: string) {
  return projectEnvironment.fileTreeEntries({ environmentId, input: { cwd } });
}

const WAITING_ENTRIES_ATOM = Atom.make(AsyncResult.initial<ProjectListEntriesResult>(true)).pipe(
  Atom.withLabel("web-project-entries:waiting"),
);

export function getProjectFileQueryAtom(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string | null,
) {
  return projectEnvironment.readFile({
    environmentId,
    input: { cwd, relativePath: relativePath ?? EMPTY_PROJECT_FILE_PATH },
  });
}

export function setProjectFileQueryData(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string,
  contents: string,
): void {
  appAtomRegistry.set(optimisticFileAtom(environmentId, cwd, relativePath), {
    confirmedAgainst: undefined,
    data: {
      relativePath,
      contents,
      byteLength: new TextEncoder().encode(contents).byteLength,
      truncated: false,
    },
  });
}

export function getOptimisticProjectFileQueryData(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string,
): ProjectReadFileResult | null {
  return appAtomRegistry.get(optimisticFileAtom(environmentId, cwd, relativePath))?.data ?? null;
}

export function confirmProjectFileQueryData(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string,
  contents: string,
): boolean {
  const atom = optimisticFileAtom(environmentId, cwd, relativePath);
  const optimisticFile = appAtomRegistry.get(atom);
  if (optimisticFile?.data.contents !== contents) return false;

  const queryAtom = getProjectFileQueryAtom(environmentId, cwd, relativePath);
  const confirmed = {
    ...optimisticFile,
    confirmedAgainst: appAtomRegistry.get(queryAtom),
  };
  appAtomRegistry.set(atom, confirmed);
  appAtomRegistry.refresh(queryAtom);
  void executeAtomQuery(appAtomRegistry, queryAtom, {
    reportDefect: false,
    reportFailure: false,
  }).then((result) => {
    if (result._tag === "Success" && appAtomRegistry.get(atom) === confirmed) {
      appAtomRegistry.set(atom, null);
    }
  });
  return true;
}

export function resolveProjectFileQueryData(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string | null,
  data: ProjectReadFileResult | null,
): ProjectReadFileResult | null {
  if (relativePath === null) return data;
  return appAtomRegistry.get(optimisticFileAtom(environmentId, cwd, relativePath))?.data ?? data;
}

export function clearProjectFileQueryData(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string,
): void {
  appAtomRegistry.set(optimisticFileAtom(environmentId, cwd, relativePath), null);
}

function errorMessage<A>(result: AsyncResult.AsyncResult<A, unknown>): string | null {
  return result._tag === "Failure"
    ? formatEnvironmentQueryError(result.cause, "Workspace query failed.")
    : null;
}

/**
 * The Files panel's entry list. While `enabled` is false nothing is read and the list shows as
 * loading; every switch to enabled reads it once.
 */
export function useProjectEntriesQuery(
  environmentId: EnvironmentId,
  cwd: string,
  enabled = true,
): ProjectQueryState<ProjectListEntriesResult> {
  const atom = enabled ? getProjectEntriesQueryAtom(environmentId, cwd) : WAITING_ENTRIES_ATOM;
  const result = useAtomValue(atom);
  const refreshAtom = useAtomRefresh(atom);
  const refresh = useCallback(() => retryEnvironmentQuery(atom, refreshAtom), [atom, refreshAtom]);
  return {
    data: Option.getOrNull(AsyncResult.value(result)),
    error: errorMessage(result),
    isPending: result.waiting,
    refresh,
    revalidate: refreshAtom,
  };
}

type EntryChangeSignal = object;

// The last entry-change signal a list read reflects, per workspace. It lives exactly as long as a
// caller of `useFileTreeEntries` holds it, like the list itself, so a fresh list starts uncovered.
const coveredSignalFamily = Atom.family((key: string) =>
  Atom.make<EntryChangeSignal | null>(null).pipe(
    Atom.setIdleTTL(0),
    Atom.withLabel(`web-file-tree:covered-signal:${key}`),
  ),
);

/**
 * What a caller of `useFileTreeEntries` does with the latest entry-change signal: `cover` records
 * it as reflected by the read that mounting the list made, `read` re-reads the list.
 *
 * The first signal is the subscription's resync, and the list is mounted only after it, so that
 * mount is the read. A later signal is re-read at once by a `live` caller (the open panel); any
 * other caller leaves it uncovered, so the next live caller re-reads on mount instead of showing a
 * stale list.
 */
export function fileTreeSignalAction(
  signal: EntryChangeSignal | null,
  covered: EntryChangeSignal | null,
  live: boolean,
): "none" | "cover" | "read" {
  if (signal === null || signal === covered) return "none";
  if (covered === null) return "cover";
  return live ? "read" : "none";
}

/**
 * The Files tree's entry list, kept in step with the server's entry-change signals.
 *
 * The list is read only once the change subscription has resynced the server's index (or failed),
 * so that read is the only one. A caller mounted ahead of the panel (`live: false`) holds the list
 * and the subscription so the panel paints at once; the panel (`live: true`) re-reads on each later
 * signal, including one that arrived while it was closed.
 */
export function useFileTreeEntries(
  environmentId: EnvironmentId,
  cwd: string,
  live: boolean,
): ProjectQueryState<ProjectListEntriesResult> {
  const entryChanges = useEnvironmentQuery(
    cwd.length > 0 ? projectEnvironment.subscribeEntries({ environmentId, input: { cwd } }) : null,
  );
  const started = cwd.length === 0 || entryChanges.data !== null || entryChanges.error !== null;
  const query = useProjectEntriesQuery(environmentId, cwd, started);
  const coveredAtom = coveredSignalFamily(JSON.stringify([environmentId, cwd]));
  useAtomValue(coveredAtom);
  const signal = entryChanges.data;
  const revalidate = query.revalidate;
  useEffect(() => {
    const action = fileTreeSignalAction(signal, appAtomRegistry.get(coveredAtom), live);
    if (action === "none" || signal === null) return;
    appAtomRegistry.set(coveredAtom, signal);
    // A signal is an automatic read, so it keeps a transport cut-off latched until a rescan.
    if (action === "read") revalidate();
  }, [coveredAtom, live, revalidate, signal]);
  return query;
}

export function useProjectFileQuery(
  environmentId: EnvironmentId,
  cwd: string,
  relativePath: string | null,
): ProjectQueryState<ProjectReadFileResult> {
  const atom = getProjectFileQueryAtom(environmentId, cwd, relativePath);
  const result = useAtomValue(atom);
  const refreshAtom = useAtomRefresh(atom);
  const refresh = useCallback(() => retryEnvironmentQuery(atom, refreshAtom), [atom, refreshAtom]);
  const data = Option.getOrNull(AsyncResult.value(result));
  const optimisticResult = useAtomValue(
    optimisticFileAtom(environmentId, cwd, relativePath ?? EMPTY_PROJECT_FILE_PATH),
  );
  const optimisticFile = relativePath === null ? null : optimisticResult;

  return {
    data: optimisticFile?.data ?? data,
    error: errorMessage(result),
    isPending: result.waiting,
    refresh,
    revalidate: refreshAtom,
  };
}
