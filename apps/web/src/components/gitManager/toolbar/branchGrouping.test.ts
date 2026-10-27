import type { GitManagerRefEntry } from "@coddy/contracts";
import { describe, expect, it } from "vite-plus/test";

import { groupBranches } from "./branchGrouping";

function branch(name: string, options: Partial<GitManagerRefEntry> = {}): GitManagerRefEntry {
  return {
    name,
    tipSha: `${name}-sha`,
    upstream: null,
    ahead: 0,
    behind: 0,
    current: false,
    isDefault: false,
    worktreePath: null,
    blocked: [],
    ...options,
  };
}

describe("groupBranches", () => {
  it("caps recent branches at five and places each branch in exactly one group", () => {
    const refs = [
      branch("main", { current: true, isDefault: true }),
      ...Array.from({ length: 7 }, (_, index) => branch(`recent-${index + 1}`)),
      branch("other"),
    ];

    const grouped = groupBranches({
      refs,
      remoteRefs: [],
      recentNames: refs.slice(1, 8).map((ref) => ref.name),
      filter: "",
    });

    expect(grouped.default.map((ref) => ref.name)).toEqual(["main"]);
    expect(grouped.recent.map((ref) => ref.name)).toEqual([
      "recent-1",
      "recent-2",
      "recent-3",
      "recent-4",
      "recent-5",
    ]);
    expect(grouped.other.map((ref) => ref.name)).toEqual(["recent-6", "recent-7", "other"]);
    expect(
      new Set([...grouped.default, ...grouped.recent, ...grouped.other].map((ref) => ref.name))
        .size,
    ).toBe(refs.length);
  });

  it("filters case-insensitively while preserving the server current marker", () => {
    const current = branch("Feature/API", { current: true });
    const grouped = groupBranches({
      refs: [branch("main", { isDefault: true }), current, branch("feature/ui")],
      remoteRefs: [],
      recentNames: ["Feature/API"],
      filter: "api",
    });

    expect(grouped).toEqual({ default: [], recent: [current], other: [], remote: [] });
    expect(grouped.recent[0]?.current).toBe(true);
  });

  it("searches remote branches without mixing them into local or recent groups", () => {
    const local = branch("origin/develop");
    const remote = branch("origin/develop");
    const upstream = branch("upstream/develop");
    expect(
      groupBranches({
        refs: [branch("main", { isDefault: true }), local],
        remoteRefs: [remote, upstream, branch("origin/release")],
        recentNames: ["origin/develop"],
        filter: "Develop",
      }),
    ).toEqual({ default: [], recent: [local], other: [], remote: [remote, upstream] });
  });
});
