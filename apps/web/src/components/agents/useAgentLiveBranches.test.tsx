// @vitest-environment happy-dom
import { EnvironmentId } from "@coddy/contracts";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  summaries: {} as Record<string, unknown>,
  reads: 0,
}));
vi.mock("../../state/vcs", async () => {
  const { AsyncResult: Result, Atom: Atoms } = await import("effect/reactivity");
  const family = Atoms.family((cwd: string) => {
    const summary = h.summaries[cwd];
    return Atoms.make(summary === undefined ? Result.initial(true) : Result.success(summary));
  });
  return {
    vcsEnvironment: {
      summary: (target: { input: { cwd: string } }) => {
        h.reads += 1;
        return family(target.input.cwd);
      },
    },
  };
});

import { useAgentLiveBranches } from "./useAgentLiveBranches";

const environmentId = EnvironmentId.make("local");
const checkout = (cwd: string) => ({
  key: JSON.stringify([environmentId, cwd]),
  environmentId,
  cwd,
});

describe("useAgentLiveBranches", () => {
  it("names each checkout's branch from its summary, stale or detached, and reuses the read", async () => {
    h.summaries = {
      "/wt/a": { refName: "feature/a", detachedHead: null, stale: false },
      "/wt/b": { refName: "feature/b", detachedHead: null, stale: true },
      "/wt/c": { refName: null, detachedHead: "abc1234", stale: false },
    };
    const seen: Array<ReadonlyMap<string, string>> = [];
    function Probe({ cwds }: { cwds: readonly string[] }) {
      seen.push(useAgentLiveBranches(cwds.map(checkout)));
      return null;
    }
    const root = createRoot(document.createElement("div"));
    try {
      await act(async () => root.render(<Probe cwds={["/wt/a", "/wt/b", "/wt/c", "/wt/d"]} />));
      expect([...seen.at(-1)!]).toEqual([
        [checkout("/wt/a").key, "feature/a"],
        [checkout("/wt/b").key, "feature/b"],
        [checkout("/wt/c").key, "abc1234"],
      ]);
      const reads = h.reads;
      // A new array of the same checkouts (every thread event rebuilds it) keeps the atom.
      await act(async () => root.render(<Probe cwds={["/wt/a", "/wt/b", "/wt/c", "/wt/d"]} />));
      expect(seen.at(-1)).toBe(seen[0]);
      expect(h.reads).toBe(reads);
    } finally {
      await act(async () => root.unmount());
    }
  });
});
