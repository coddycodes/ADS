import type { EnvironmentThreadShell } from "@bibcode/client-runtime/state/models";
import { describe, expect, it } from "vite-plus/test";

import { rankBesideThreadCandidates } from "./openBesideThread.logic";

const thread = (
  id: string,
  branch: string | null,
  updatedAt: string,
  archivedAt: string | null = null,
) =>
  ({
    id,
    environmentId: "env",
    title: `Thread ${id}`,
    branch,
    updatedAt,
    archivedAt,
  }) as unknown as EnvironmentThreadShell;

describe("rankBesideThreadCandidates", () => {
  it("lists threads on the request's branch first, then the most recent, without archived ones", () => {
    const ranked = rankBesideThreadCandidates(
      [
        thread("old", "main", "2026-10-01T00:00:00.000Z"),
        thread("branch-old", "feature/x", "2026-10-02T00:00:00.000Z"),
        thread("new", null, "2026-10-14T00:00:00.000Z"),
        thread("archived", "feature/x", "2026-10-15T00:00:00.000Z", "2026-10-15T01:00:00.000Z"),
        thread("branch-new", "feature/x", "2026-10-10T00:00:00.000Z"),
      ],
      "feature/x",
    );
    expect(ranked.map((candidate) => [candidate.title, candidate.onBranch])).toEqual([
      ["Thread branch-new", true],
      ["Thread branch-old", true],
      ["Thread new", false],
      ["Thread old", false],
    ]);
    expect(ranked[0]!.ref).toEqual({ environmentId: "env", threadId: "branch-new" });
  });

  it("returns nothing for a project without live threads", () => {
    expect(rankBesideThreadCandidates([], "feature/x")).toEqual([]);
  });
});
