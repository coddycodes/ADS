import { describe, expect, it } from "vite-plus/test";
import {
  buildSmartRows,
  canReuseBranch,
  detectSmartMode,
  filterRefsByQuery,
  findExactRefMatch,
  getCreateWorktreeDisabled,
  githubWorkItemBranchName,
  parseGitHubWorkItem,
  resolveWorktreeCreateInput,
  reuseBranchHint,
  sanitizeBranchName,
  suggestNextAvailableBranchName,
  suggestWorktreeNameFromRef,
  type RefLike,
} from "./CreateWorktreeDialog.logic";

describe("parseGitHubWorkItem", () => {
  it("parses a bare number", () => {
    expect(parseGitHubWorkItem("123")).toEqual({ number: 123, kind: "unknown" });
  });

  it("parses a #-prefixed number", () => {
    expect(parseGitHubWorkItem("#456")).toEqual({ number: 456, kind: "unknown" });
  });

  it("parses a github issues URL", () => {
    expect(parseGitHubWorkItem("https://github.com/acme/widgets/issues/789")).toEqual({
      number: 789,
      kind: "issue",
    });
  });

  it("parses a github pull URL", () => {
    expect(parseGitHubWorkItem("https://github.com/acme/widgets/pull/42")).toEqual({
      number: 42,
      kind: "pr",
    });
  });

  it("trims whitespace", () => {
    expect(parseGitHubWorkItem("  #7  ")).toEqual({ number: 7, kind: "unknown" });
  });

  it("returns null for non-matching input", () => {
    expect(parseGitHubWorkItem("feature/my-branch")).toBeNull();
    expect(parseGitHubWorkItem("")).toBeNull();
    expect(parseGitHubWorkItem("   ")).toBeNull();
  });

  it("rejects zero/negative-looking numbers", () => {
    expect(parseGitHubWorkItem("0")).toBeNull();
  });
});

describe("githubWorkItemBranchName", () => {
  it("derives pr-<n> for any kind", () => {
    expect(githubWorkItemBranchName({ number: 123, kind: "issue" })).toBe("pr-123");
    expect(githubWorkItemBranchName({ number: 5, kind: "pr" })).toBe("pr-5");
    expect(githubWorkItemBranchName({ number: 9, kind: "unknown" })).toBe("pr-9");
  });
});

describe("sanitizeBranchName", () => {
  it("replaces whitespace with dashes", () => {
    expect(sanitizeBranchName("my new feature")).toBe("my-new-feature");
  });

  it("strips disallowed characters", () => {
    expect(sanitizeBranchName("fix: bug#42!")).toBe("fix-bug-42");
  });

  it("collapses repeated dashes", () => {
    expect(sanitizeBranchName("a---b")).toBe("a-b");
  });

  it("trims leading/trailing separators", () => {
    expect(sanitizeBranchName("  /feature/ ")).toBe("feature");
  });

  it("preserves slashes and dots inside the name", () => {
    expect(sanitizeBranchName("feature/sub.task")).toBe("feature/sub.task");
  });
});

describe("filterRefsByQuery", () => {
  const refs: RefLike[] = [
    { name: "main" },
    { name: "origin/main" },
    { name: "feature/login" },
    { name: "feature/logout" },
  ];

  it("returns all refs for an empty query", () => {
    expect(filterRefsByQuery(refs, "")).toHaveLength(4);
  });

  it("filters case-insensitively by substring", () => {
    expect(filterRefsByQuery(refs, "LOGIN")).toEqual([{ name: "feature/login" }]);
  });

  it("returns empty array when nothing matches", () => {
    expect(filterRefsByQuery(refs, "zzz")).toEqual([]);
  });
});

describe("canReuseBranch", () => {
  it("accepts only a free local branch", () => {
    expect(canReuseBranch({ name: "feature" })).toBe(true);
    expect(canReuseBranch({ name: "origin/feature", isRemote: true })).toBe(false);
    expect(canReuseBranch({ name: "main", current: true })).toBe(false);
    expect(canReuseBranch({ name: "feature", worktreePath: "/repo-feature" })).toBe(false);
  });
});

describe("reuseBranchHint", () => {
  it("explains the enabled state for a free local branch", () => {
    expect(
      reuseBranchHint({
        name: "feature/free",
        isRemote: false,
        current: false,
        worktreePath: null,
      }),
    ).toBe("Check out the existing branch instead of creating a new one from it.");
  });

  it("explains that a remote branch yields a new local branch", () => {
    expect(
      reuseBranchHint({ name: "origin/main", isRemote: true, current: false, worktreePath: null }),
    ).toBe('"origin/main" is a remote branch. A local branch will be created from it.');
  });

  it("explains that the current branch is already checked out", () => {
    expect(
      reuseBranchHint({ name: "main", isRemote: false, current: true, worktreePath: null }),
    ).toBe(
      '"main" is already checked out. A new branch ("main-2" or the next available name) will be created from it.',
    );
  });

  it("names the branch an edited Name creates from an occupied branch", () => {
    expect(reuseBranchHint({ name: "master", current: true }, "my-feature")).toBe(
      '"master" is already checked out. A new branch ("my-feature" or the next available name) will be created from it.',
    );
    expect(reuseBranchHint({ name: "master", current: true }, "master")).toBe(
      '"master" is already checked out. A new branch ("master-2" or the next available name) will be created from it.',
    );
  });

  it("explains that a branch checked out in another worktree is occupied", () => {
    expect(
      reuseBranchHint({ name: "wip", isRemote: false, current: false, worktreePath: "/repo/wip" }),
    ).toBe(
      '"wip" is already checked out. A new branch ("wip-2" or the next available name) will be created from it.',
    );
  });
});

describe("suggestNextAvailableBranchName", () => {
  it("uses the first available numeric suffix", () => {
    expect(
      suggestNextAvailableBranchName("feature/login", [
        { name: "feature/login" },
        { name: "feature/login-2" },
        { name: "origin/feature/login-3", isRemote: true },
      ]),
    ).toBe("feature/login-3");
  });
});

describe("suggestWorktreeNameFromRef", () => {
  it("drops only the remote name prefix", () => {
    expect(suggestWorktreeNameFromRef({ name: "origin/feature/login", isRemote: true })).toBe(
      "feature/login",
    );
    expect(
      suggestWorktreeNameFromRef({
        name: "team/origin/feature/login",
        isRemote: true,
        remoteName: "team/origin",
      }),
    ).toBe("feature/login");
  });
});

describe("findExactRefMatch", () => {
  const refs: RefLike[] = [{ name: "main" }, { name: "feature/login" }];

  it("finds an exact case-sensitive match", () => {
    expect(findExactRefMatch(refs, "main")).toEqual({ name: "main" });
  });

  it("returns null when no exact match", () => {
    expect(findExactRefMatch(refs, "Main")).toBeNull();
    expect(findExactRefMatch(refs, "feat")).toBeNull();
  });

  it("returns null for empty query", () => {
    expect(findExactRefMatch(refs, "  ")).toBeNull();
  });
});

describe("buildSmartRows", () => {
  const refs: RefLike[] = [{ name: "feature/login" }, { name: "feature/logout" }, { name: "main" }];

  it("returns empty rows for empty query", () => {
    expect(buildSmartRows({ query: "", refs })).toEqual([]);
  });

  it("pins a github row first when input parses as a work item", () => {
    const rows = buildSmartRows({ query: "#123", refs });
    expect(rows[0]).toEqual({ kind: "github", item: { number: 123, kind: "unknown" } });
  });

  it("shows only matching branches when the query is not a GitHub item", () => {
    const rows = buildSmartRows({ query: "feature", refs });
    expect(rows).toEqual([
      { kind: "branch", refName: "feature/login" },
      { kind: "branch", refName: "feature/logout" },
    ]);
  });

  it("caps branch rows at maxBranchRows", () => {
    const manyRefs: RefLike[] = Array.from({ length: 10 }, (_, i) => ({ name: `feature/${i}` }));
    const rows = buildSmartRows({ query: "feature", refs: manyRefs, maxBranchRows: 3 });
    expect(rows.filter((r) => r.kind === "branch")).toHaveLength(3);
  });
});

describe("detectSmartMode", () => {
  const refs: RefLike[] = [{ name: "feature/login" }, { name: "main" }];

  it("returns search for empty query", () => {
    expect(detectSmartMode("", refs)).toBe("search");
  });

  it("detects github pattern", () => {
    expect(detectSmartMode("#123", refs)).toBe("github");
  });

  it("detects exact ref match as branch", () => {
    expect(detectSmartMode("main", refs)).toBe("branch");
  });

  it("detects prefix ref match as branch", () => {
    expect(detectSmartMode("feature/lo", refs)).toBe("branch");
  });

  it("falls back to search when nothing matches", () => {
    expect(detectSmartMode("brand-new-thing", refs)).toBe("search");
  });
});

describe("resolveWorktreeCreateInput", () => {
  const base = {
    nameText: "",
    sourceText: "",
    selectedBranchRefName: null,
    selectedBranchRef: null,
    reuseSelectedBranch: false,
    githubItem: null,
    advancedBaseBranchOverride: null,
    defaultBaseBranch: "main",
  };

  it("creates a named branch from the default base when Create From is empty", () => {
    const result = resolveWorktreeCreateInput({ ...base, mode: "smart", nameText: "My Feature" });
    expect(result).toMatchObject({
      title: "My Feature",
      refName: "main",
      newRefName: "My-Feature",
      baseRefName: "main",
    });
  });

  it("returns null when Name is empty", () => {
    expect(resolveWorktreeCreateInput({ ...base, mode: "smart", nameText: "   " })).toBeNull();
  });

  it("requires Name even when a reusable branch is selected", () => {
    expect(
      resolveWorktreeCreateInput({
        ...base,
        mode: "branch",
        selectedBranchRefName: "feature/login",
        selectedBranchRef: { name: "feature/login" },
        reuseSelectedBranch: true,
      }),
    ).toBeNull();
  });

  it("resolves branch mode as an existing ref", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "branch",
      nameText: "feature--login",
      selectedBranchRefName: "feature--login",
      selectedBranchRef: { name: "feature--login" },
      reuseSelectedBranch: true,
      advancedBaseBranchOverride: "develop",
    });
    expect(result).toEqual({
      kind: "existing-ref",
      title: "feature--login",
      branchName: "feature--login",
      refName: "feature--login",
      newRefName: null,
      baseRefName: null,
    });
  });

  it("creates the edited Name as a new branch from an occupied branch (#136)", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "branch",
      nameText: "my-feature",
      selectedBranchRefName: "master",
      selectedBranchRef: { name: "master", current: true },
    });
    expect(result).toEqual({
      kind: "new-branch",
      title: "my-feature",
      branchName: "my-feature",
      refName: "master",
      newRefName: "my-feature",
      baseRefName: "master",
    });
  });

  it("leaves an occupied branch's own name to the server's next-available suffix", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "branch",
      nameText: "wip",
      selectedBranchRefName: "wip",
      selectedBranchRef: { name: "wip", worktreePath: "/repo/wip" },
    });
    expect(result).toMatchObject({ kind: "existing-ref", refName: "wip", newRefName: null });
  });

  it("creates a named branch from a selected reusable branch when reuse is off", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "branch",
      nameText: "Login polish",
      selectedBranchRefName: "feature/login",
      selectedBranchRef: { name: "feature/login", isRemote: false },
    });
    expect(result).toMatchObject({
      title: "Login polish",
      refName: "feature/login",
      newRefName: "Login-polish",
      baseRefName: "feature/login",
    });
  });

  it("never reuses a remote branch even when stale UI state says reuse is on", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "branch",
      nameText: "Login polish",
      selectedBranchRefName: "origin/feature/login",
      selectedBranchRef: { name: "origin/feature/login", isRemote: true },
      reuseSelectedBranch: true,
    });
    expect(result).toMatchObject({
      refName: "origin/feature/login",
      newRefName: "Login-polish",
      baseRefName: "origin/feature/login",
    });
  });

  it("returns null for branch mode with no selection", () => {
    expect(resolveWorktreeCreateInput({ ...base, mode: "branch" })).toBeNull();
  });

  it("treats an empty Create From branch search as optional", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "branch",
      nameText: "New workspace",
    });
    expect(result).toMatchObject({
      refName: "main",
      newRefName: "New-workspace",
      baseRefName: "main",
    });
  });

  it("resolves github mode from the parsed item", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "github",
      nameText: "pr-42",
      githubItem: { number: 42, kind: "pr" },
    });
    expect(result).toEqual({
      kind: "new-branch",
      title: "pr-42",
      branchName: "pr-42",
      refName: "main",
      newRefName: "pr-42",
      baseRefName: "main",
    });
  });

  it("uses an edited Name for a GitHub source", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "github",
      nameText: "Fix login",
      sourceText: "#42",
      githubItem: { number: 42, kind: "pr" },
    });
    expect(result).toMatchObject({
      title: "Fix login",
      newRefName: "Fix-login",
    });
  });

  it("returns null for github mode with no parsed item", () => {
    expect(resolveWorktreeCreateInput({ ...base, mode: "github" })).toBeNull();
  });

  it("requires Name when a GitHub source resolves", () => {
    expect(
      resolveWorktreeCreateInput({
        ...base,
        mode: "github",
        sourceText: "#42",
        githubItem: { number: 42, kind: "pr" },
      }),
    ).toBeNull();
  });

  it("treats an empty Create From GitHub input as optional", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "github",
      nameText: "New workspace",
    });
    expect(result).toMatchObject({
      refName: "main",
      newRefName: "New-workspace",
      baseRefName: "main",
    });
  });

  it("smart mode prefers a github item over a branch selection", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "smart",
      nameText: "pr-7",
      githubItem: { number: 7, kind: "issue" },
      selectedBranchRefName: "feature/login",
    });
    expect(result).toEqual({
      kind: "new-branch",
      title: "pr-7",
      branchName: "pr-7",
      refName: "main",
      newRefName: "pr-7",
      baseRefName: "main",
    });
  });

  it("smart mode falls back to an existing ref when no github item", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "smart",
      nameText: "feature--login",
      selectedBranchRefName: "feature--login",
      selectedBranchRef: { name: "feature--login" },
      reuseSelectedBranch: true,
      advancedBaseBranchOverride: "develop",
    });
    expect(result).toEqual({
      kind: "existing-ref",
      title: "feature--login",
      branchName: "feature--login",
      refName: "feature--login",
      newRefName: null,
      baseRefName: null,
    });
  });

  it("smart mode creates from its selected branch when reuse is off", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "smart",
      nameText: "Login polish",
      selectedBranchRefName: "feature/login",
      selectedBranchRef: { name: "feature/login" },
    });
    expect(result).toMatchObject({
      refName: "feature/login",
      newRefName: "Login-polish",
      baseRefName: "feature/login",
    });
  });

  it("smart mode falls back to sanitized text when nothing else resolves", () => {
    const result = resolveWorktreeCreateInput({ ...base, mode: "smart", nameText: "brand new" });
    expect(result).toEqual({
      kind: "new-branch",
      title: "brand new",
      branchName: "brand-new",
      refName: "main",
      newRefName: "brand-new",
      baseRefName: "main",
    });
  });

  it("rejects a non-empty Smart search without a current source selection", () => {
    expect(
      resolveWorktreeCreateInput({
        ...base,
        mode: "smart",
        nameText: "Workspace",
        sourceText: "feature/login",
      }),
    ).toBeNull();
  });

  it("uses HEAD as the base when no default base branch is available", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "smart",
      nameText: "feature",
      defaultBaseBranch: null,
    });
    expect(result).toEqual({
      kind: "new-branch",
      title: "feature",
      branchName: "feature",
      refName: "HEAD",
      newRefName: "feature",
      baseRefName: "HEAD",
    });
  });

  it("prefers the advanced base-branch override over the default", () => {
    const result = resolveWorktreeCreateInput({
      ...base,
      mode: "smart",
      nameText: "feature",
      advancedBaseBranchOverride: "develop",
    });
    expect(result).toEqual({
      kind: "new-branch",
      title: "feature",
      branchName: "feature",
      refName: "develop",
      newRefName: "feature",
      baseRefName: "develop",
    });
  });
});

describe("getCreateWorktreeDisabled", () => {
  const resolution = {
    kind: "new-branch" as const,
    title: "feature",
    branchName: "feature",
    refName: "main",
    newRefName: "feature",
    baseRefName: "main",
  };

  it("is disabled without a project", () => {
    expect(getCreateWorktreeDisabled({ hasProject: false, resolution, isSubmitting: false })).toBe(
      true,
    );
  });

  it("is disabled without a resolution", () => {
    expect(
      getCreateWorktreeDisabled({ hasProject: true, resolution: null, isSubmitting: false }),
    ).toBe(true);
  });

  it("is disabled while submitting", () => {
    expect(getCreateWorktreeDisabled({ hasProject: true, resolution, isSubmitting: true })).toBe(
      true,
    );
  });

  it("is enabled when project + resolution present and not submitting", () => {
    expect(getCreateWorktreeDisabled({ hasProject: true, resolution, isSubmitting: false })).toBe(
      false,
    );
  });
});
