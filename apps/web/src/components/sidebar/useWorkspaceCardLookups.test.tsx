// @vitest-environment happy-dom
import { scopeThreadRef, scopedThreadKey } from "@coddy/client-runtime/environment";
import { EnvironmentId, ThreadId } from "@coddy/contracts";
import { act, memo } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import * as indexes from "./workspaceCardLookups";
import type { SidebarProjectSnapshot } from "../../sidebarProjectGrouping";

// Each source atom is labelled; the mock answers per source so a test can change
// one observation (the catalog) while the others keep their references.
const fixture = vi.hoisted(() => ({
  sources: { adopted: [], terminals: [], ports: [] } as Record<string, readonly unknown[]>,
}));
vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: { label?: readonly [string, string] }) => {
    const name = atom.label?.[0]?.replace("workspace-card-", "") ?? "";
    const value = fixture.sources[name];
    if (value === undefined) throw new Error(`unlabelled card source atom: ${name}`);
    return value;
  },
}));
vi.mock("../../state/terminal", () => ({ terminalEnvironment: {} }));
vi.mock("../../state/preview", () => ({ previewEnvironment: {} }));
vi.mock("../../state/worktrees", () => ({ worktreeEnvironment: {} }));

import { useWorkspaceCardLookups } from "./useWorkspaceCardLookups";
afterEach(() => vi.restoreAllMocks());

describe("list-wide card lookups", () => {
  it("builds each Map once for a list render, reuses references, and never rebuilds per card", async () => {
    const builds = [
      vi.spyOn(indexes, "buildWorkspaceCardKeys"),
      vi.spyOn(indexes, "buildAdoptedWorkspaceMap"),
      vi.spyOn(indexes, "buildRunningTerminalMap"),
      vi.spyOn(indexes, "buildDiscoveredPortMap"),
    ];
    const environmentId = EnvironmentId.make("local");
    const threads = Array.from({ length: 50 }, (_, i) => ({
      id: ThreadId.make("thread-" + i),
      environmentId,
    }));
    const input = {
      project: { memberProjects: [] } as unknown as SidebarProjectSnapshot,
      serverConfigs: new Map(),
      active: true,
      threads,
      pinnedKeys: [] as string[],
      unreadKeys: [] as string[],
    };
    const seen: ReturnType<typeof useWorkspaceCardLookups>[] = [];
    function List() {
      const maps = useWorkspaceCardLookups(input);
      seen.push(maps);
      return (
        <ul>
          {threads.map((thread) => {
            const key = maps.keys.get(thread)!.key;
            return <li key={key}>{maps.terminals.get(key)?.length ?? 0}</li>;
          })}
        </ul>
      );
    }
    const container = document.createElement("div");
    const root = createRoot(container);
    try {
      await act(async () => root.render(<List />));
      expect(container.querySelectorAll("li")).toHaveLength(50);
      for (const build of builds) expect(build).toHaveBeenCalledTimes(1);
      await act(async () => root.render(<List />));
      for (const build of builds) expect(build).toHaveBeenCalledTimes(1);
      expect(seen.at(-1)).toBe(seen[0]);
    } finally {
      await act(async () => root.unmount());
    }
  });

  it("keeps equal thread ids in different environments separate and drops idle terminals", () => {
    const local = EnvironmentId.make("local");
    const remote = EnvironmentId.make("remote");
    const id = ThreadId.make("same");
    const key = (environmentId: typeof local) => scopedThreadKey(scopeThreadRef(environmentId, id));
    const terminal = (terminalId: string, hasRunningSubprocess: boolean) => ({
      threadId: id,
      terminalId,
      cwd: "/repo",
      worktreePath: null,
      status: "running" as const,
      pid: null,
      exitCode: null,
      exitSignal: null,
      hasRunningSubprocess,
      label: "",
      updatedAt: "2026-09-24T00:00:00Z",
      centerPanel: false,
    });
    const map = indexes.buildRunningTerminalMap([
      { environmentId: local, rows: [terminal("active", true), terminal("idle", false)] },
      { environmentId: remote, rows: [terminal("remote", true)] },
    ]);
    expect(map.get(key(local))).toEqual(["active"]);
    expect(map.get(key(remote))).toEqual(["remote"]);
    const port = {
      host: "localhost",
      port: 3000,
      url: "http://localhost:3000",
      processName: null,
      pid: null,
      terminal: { threadId: id, terminalId: "active" },
    };
    const ports = indexes.buildDiscoveredPortMap([
      { environmentId: local, rows: [port] },
      { environmentId: remote, rows: [{ ...port, port: 4000 }] },
    ]);
    expect(ports.get(key(local))?.[0]?.port).toBe(3000);
    expect(ports.get(key(remote))?.[0]?.port).toBe(4000);
  });

  it("keeps card inputs stable when only the worktree catalog changes", async () => {
    const environmentId = EnvironmentId.make("local");
    const threadId = ThreadId.make("thread-a");
    const otherId = ThreadId.make("thread-b");
    const key = scopedThreadKey(scopeThreadRef(environmentId, threadId));
    const terminal = (id: typeof threadId, terminalId: string) => ({
      threadId: id,
      terminalId,
      cwd: "/repo",
      worktreePath: null,
      status: "running" as const,
      pid: null,
      exitCode: null,
      exitSignal: null,
      hasRunningSubprocess: true,
      label: "",
      updatedAt: "2026-09-24T00:00:00Z",
    });
    const port = {
      host: "localhost",
      port: 3000,
      url: "http://localhost:3000",
      processName: null,
      pid: null,
      terminal: { threadId, terminalId: "t1" },
    };
    const adoptedRow = (generation: number) => ({
      threadId,
      path: "/wt/a",
      branch: "a",
      availability: "present",
      registrationState: "registered",
      locked: false,
      generation,
    });
    fixture.sources = {
      adopted: [{ environmentId, rows: [adoptedRow(1)] }],
      terminals: [{ environmentId, rows: [terminal(threadId, "t1")] }],
      ports: [{ environmentId, rows: [port] }],
    };
    const terminalBuild = vi.spyOn(indexes, "buildRunningTerminalMap");
    const portBuild = vi.spyOn(indexes, "buildDiscoveredPortMap");
    const threads = [{ id: threadId, environmentId }];
    const input = {
      project: { memberProjects: [] } as unknown as SidebarProjectSnapshot,
      serverConfigs: new Map(),
      active: true,
      threads,
      pinnedKeys: [] as string[],
      unreadKeys: [] as string[],
    };
    const cardRendered = vi.fn();
    const Card = memo(function Card(props: {
      terminals: readonly string[];
      ports: readonly unknown[];
      adopted: unknown;
    }) {
      cardRendered();
      return <li>{props.terminals.length + props.ports.length + (props.adopted ? 1 : 0)}</li>;
    });
    const seen: ReturnType<typeof useWorkspaceCardLookups>[] = [];
    function List() {
      const maps = useWorkspaceCardLookups(input);
      seen.push(maps);
      return (
        <ul>
          <Card
            terminals={maps.terminals.get(key)!}
            ports={maps.ports.get(key)!}
            adopted={maps.adopted.get(key)}
          />
        </ul>
      );
    }
    const container = document.createElement("div");
    const root = createRoot(container);
    try {
      await act(async () => root.render(<List />));
      const first = seen.at(-1)!;
      // A new catalog generation: new arrays and row objects, same row contents
      // except the generation, while terminals and ports keep their references.
      fixture.sources = { ...fixture.sources, adopted: [{ environmentId, rows: [adoptedRow(1)] }] };
      await act(async () => root.render(<List />));
      const second = seen.at(-1)!;
      expect(terminalBuild).toHaveBeenCalledTimes(1);
      expect(portBuild).toHaveBeenCalledTimes(1);
      expect(second.terminals.get(key)).toBe(first.terminals.get(key));
      expect(second.ports.get(key)).toBe(first.ports.get(key));
      expect(second.adopted.get(key)).toBe(first.adopted.get(key));
      expect(cardRendered).toHaveBeenCalledTimes(1);
      // Another thread's terminal starts: the map rebuilds, this card's array is reused.
      fixture.sources = {
        ...fixture.sources,
        terminals: [{ environmentId, rows: [terminal(threadId, "t1"), terminal(otherId, "t2")] }],
      };
      await act(async () => root.render(<List />));
      expect(terminalBuild).toHaveBeenCalledTimes(2);
      expect(seen.at(-1)!.terminals.get(key)).toBe(first.terminals.get(key));
      expect(cardRendered).toHaveBeenCalledTimes(1);
      // The same terminal snapshot, freshly decoded: new row objects, equal content.
      fixture.sources = {
        ...fixture.sources,
        terminals: [{ environmentId, rows: [terminal(threadId, "t1"), terminal(otherId, "t2")] }],
      };
      await act(async () => root.render(<List />));
      expect(terminalBuild).toHaveBeenCalledTimes(3);
      expect(seen.at(-1)!.terminals.get(key)).toBe(first.terminals.get(key));
      expect(cardRendered).toHaveBeenCalledTimes(1);
      // The same port snapshot, freshly decoded: new port and terminal objects.
      fixture.sources = {
        ...fixture.sources,
        ports: [{ environmentId, rows: [{ ...port, terminal: { ...port.terminal } }] }],
      };
      await act(async () => root.render(<List />));
      expect(portBuild).toHaveBeenCalledTimes(2);
      expect(seen.at(-1)!.ports.get(key)).toBe(first.ports.get(key));
      expect(cardRendered).toHaveBeenCalledTimes(1);
      // A field the card reads changes (the port's terminal): the card gets new input.
      fixture.sources = {
        ...fixture.sources,
        ports: [{ environmentId, rows: [{ ...port, terminal: { threadId, terminalId: "t9" } }] }],
      };
      await act(async () => root.render(<List />));
      expect(seen.at(-1)!.ports.get(key)).not.toBe(first.ports.get(key));
      expect(seen.at(-1)!.ports.get(key)?.[0]?.terminal?.terminalId).toBe("t9");
      expect(cardRendered).toHaveBeenCalledTimes(2);
    } finally {
      await act(async () => root.unmount());
    }
  });
});
