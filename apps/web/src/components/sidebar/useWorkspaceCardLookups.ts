import { useAtomValue } from "@effect/atom-react";
import type { DiscoveredLocalServer, EnvironmentId, ServerConfig } from "@bibcode/contracts";
import * as Option from "effect/Option";
import { AsyncResult, Atom } from "effect/reactivity";
import { useMemo, useRef } from "react";

import type { SidebarProjectSnapshot } from "../../sidebarProjectGrouping";
import { selectWorktreeCatalogCapabilityPolicy } from "@bibcode/client-runtime/state/worktrees";
import { previewEnvironment } from "../../state/preview";
import { terminalEnvironment } from "../../state/terminal";
import { worktreeEnvironment } from "../../state/worktrees";
import * as indexes from "./workspaceCardLookups";

type Sources = indexes.WorkspaceCardSources;

function uniqueEnvironments(project: SidebarProjectSnapshot): EnvironmentId[] {
  return [...new Set(project.memberProjects.map((member) => member.environmentId))];
}

/**
 * Reuses the previous value for every key whose new value is equal, so a
 * rebuilt Map hands unchanged cards the same references and memoised cards skip.
 */
function useStableMapValues<V>(
  next: ReadonlyMap<string, V>,
  equal: (left: V, right: V) => boolean,
): ReadonlyMap<string, V> {
  const previousRef = useRef<ReadonlyMap<string, V> | null>(null);
  return useMemo(() => {
    const previous = previousRef.current;
    let result: ReadonlyMap<string, V> = next;
    if (previous !== null) {
      const merged = new Map<string, V>();
      let changed = previous.size !== next.size;
      for (const [key, value] of next) {
        const old = previous.get(key);
        if (old !== undefined && equal(old, value)) {
          merged.set(key, old);
        } else {
          merged.set(key, value);
          changed = true;
        }
      }
      result = changed ? merged : previous;
    }
    previousRef.current = result;
    return result;
    // `equal` is a module-level function.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [next]);
}

/** Terminal map values are terminal-id strings, so element identity is value equality. */
function shallowEqualArrays<T>(left: readonly T[], right: readonly T[]): boolean {
  return (
    left.length === right.length && left.every((value, index) => Object.is(value, right[index]))
  );
}

/**
 * Field-by-field port equality over everything the card reads (the first
 * port's number, and every field `openDiscoveredPort` uses), so a freshly
 * decoded but identical snapshot keeps the card's array.
 */
function equalDiscoveredPort(left: DiscoveredLocalServer, right: DiscoveredLocalServer): boolean {
  return (
    left.host === right.host &&
    left.port === right.port &&
    left.url === right.url &&
    left.processName === right.processName &&
    left.pid === right.pid &&
    (left.terminal === right.terminal ||
      (left.terminal !== null &&
        right.terminal !== null &&
        left.terminal.threadId === right.terminal.threadId &&
        left.terminal.terminalId === right.terminal.terminalId))
  );
}

function equalDiscoveredPorts(
  left: readonly DiscoveredLocalServer[],
  right: readonly DiscoveredLocalServer[],
): boolean {
  return (
    left.length === right.length &&
    left.every((port, index) => equalDiscoveredPort(port, right[index]!))
  );
}

/** Adopted worktree rows hold only primitive fields, so a shallow compare is field by field. */
function shallowEqualRecords(left: object, right: object): boolean {
  const leftEntries = Object.entries(left);
  if (leftEntries.length !== Object.keys(right).length) return false;
  return leftEntries.every(([key, value]) =>
    Object.is(value, (right as Record<string, unknown>)[key]),
  );
}

export function useWorkspaceCardLookups(input: {
  readonly project: SidebarProjectSnapshot;
  readonly serverConfigs: ReadonlyMap<EnvironmentId, ServerConfig>;
  readonly active: boolean;
  readonly threads: readonly indexes.CardKeyThread[];
  readonly pinnedKeys: readonly string[];
  readonly unreadKeys: readonly string[];
}) {
  const { project, serverConfigs, active, threads, pinnedKeys, unreadKeys } = input;
  // One atom per source, so a catalog emission never rebuilds terminal or port
  // lookups (and vice versa). Each reads only its own per-environment atoms.
  const { adoptedAtom, terminalsAtom, portsAtom } = useMemo(() => {
    const adoptedAtom = Atom.make((get): Sources["adopted"] => {
      if (!active) return indexes.EMPTY_CARD_SOURCES.adopted;
      const adopted: Array<Sources["adopted"][number]> = [];
      for (const member of project.memberProjects) {
        const config = serverConfigs.get(member.environmentId);
        if (
          !config ||
          selectWorktreeCatalogCapabilityPolicy(config.environment).catalogRpc !== "enabled"
        ) {
          continue;
        }
        const catalog = Option.getOrNull(
          AsyncResult.value(
            get(
              worktreeEnvironment.catalog({
                environmentId: member.environmentId,
                input: { projectId: member.id },
              }),
            ),
          ),
        );
        adopted.push({
          environmentId: member.environmentId,
          rows: catalog?.adoptedWorkspaces ?? [],
        });
      }
      return adopted;
    }).pipe(Atom.withLabel("workspace-card-adopted"));
    const terminalsAtom = Atom.make((get): Sources["terminals"] => {
      if (!active) return indexes.EMPTY_CARD_SOURCES.terminals;
      return uniqueEnvironments(project).map((environmentId) => ({
        environmentId,
        rows:
          Option.getOrNull(
            AsyncResult.value(get(terminalEnvironment.metadata({ environmentId, input: null }))),
          ) ?? [],
      }));
    }).pipe(Atom.withLabel("workspace-card-terminals"));
    const portsAtom = Atom.make((get): Sources["ports"] => {
      if (!active) return indexes.EMPTY_CARD_SOURCES.ports;
      return uniqueEnvironments(project).map((environmentId) => ({
        environmentId,
        rows:
          Option.getOrNull(
            AsyncResult.value(
              get(previewEnvironment.discoveredServers({ environmentId, input: {} })),
            ),
          )?.servers ?? [],
      }));
    }).pipe(Atom.withLabel("workspace-card-ports"));
    return { adoptedAtom, terminalsAtom, portsAtom };
    // `project` is read only through its member list.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, project.memberProjects, serverConfigs]);
  const adoptedSources = useAtomValue(adoptedAtom);
  const terminalSources = useAtomValue(terminalsAtom);
  const portSources = useAtomValue(portsAtom);
  const keys = useMemo(
    () => indexes.buildWorkspaceCardKeys(threads, pinnedKeys, unreadKeys),
    [threads, pinnedKeys, unreadKeys],
  );
  const adopted = useStableMapValues(
    useMemo(() => indexes.buildAdoptedWorkspaceMap(adoptedSources), [adoptedSources]),
    shallowEqualRecords,
  );
  const terminals = useStableMapValues(
    useMemo(() => indexes.buildRunningTerminalMap(terminalSources), [terminalSources]),
    shallowEqualArrays,
  );
  const ports = useStableMapValues(
    useMemo(() => indexes.buildDiscoveredPortMap(portSources), [portSources]),
    equalDiscoveredPorts,
  );
  return useMemo(() => ({ keys, adopted, terminals, ports }), [keys, adopted, terminals, ports]);
}
