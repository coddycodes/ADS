import {
  EnvironmentId,
  ProjectId,
  type GitManagerCommitEntry,
  type GitManagerRefEntry,
  type GitManagerStashEntry,
  type ScopedProjectRef,
  type GitManagerDiff,
  type GitManagerGetDiffInput,
  type GitManagerRemoteTags,
  type GitManagerRemoteTagsInput,
  type GitManagerMergePreview,
  type GitManagerPreviewMergeInput,
  type GitManagerRefsSnapshot,
  type GitManagerCommitPage,
  type PullRequestsVocabulary,
  type PullRequestsVocabularyInput,
  type PullRequestsCreateDefaults,
  type VcsListRefsResult,
  type VcsStatusResult,
} from "@bibcode/contracts";

export const environmentId = EnvironmentId.make("issue27-owned-git-fixture");
export const projectRef: ScopedProjectRef = {
  environmentId,
  projectId: ProjectId.make("issue27-owned-git"),
};
export const scope = (id: string) => ({ environmentId, cwd: `/component-fixtures/issue27/${id}` });
export const sha = "a".repeat(40);
export const otherSha = "b".repeat(40);
export function branch(name: string, patch: Partial<GitManagerRefEntry> = {}): GitManagerRefEntry {
  return {
    name,
    tipSha: sha,
    upstream: null,
    ahead: 0,
    behind: 0,
    current: false,
    isDefault: false,
    worktreePath: null,
    blocked: [],
    ...patch,
  };
}
export const refs: readonly GitManagerRefEntry[] = [
  branch("main", { current: true, isDefault: true }),
  branch("feature/typography", {
    tipSha: otherSha,
    upstream: "origin/feature/typography",
    ahead: 3,
    behind: 2,
  }),
  branch("release/next", { tipSha: "c".repeat(40) }),
];
export const remoteRefs = [
  branch("origin/main"),
  branch("origin/release/next", { tipSha: otherSha }),
];
export const commit: GitManagerCommitEntry = {
  sha,
  shortSha: "aaaaaaa",
  parents: [otherSha],
  decorations: ["HEAD -> main", "tag: v0.8.2"],
  subject: "Keep secondary Git text readable",
  body: "Preserve readable typography in compact repository controls.",
  authorName: "Local Author",
  authorEmail: "author@example.test",
  authoredAtMs: Date.UTC(2026, 8, 20, 10),
  committerName: "Local Committer",
  committerEmail: "committer@example.test",
  committedAtMs: Date.UTC(2026, 8, 20, 11),
  changedFiles: ["src/readable.ts", "docs/typography.md"],
};
export const entries: readonly GitManagerStashEntry[] = [
  {
    index: 0,
    sha,
    message: "Typography work before branch switch",
    committedAtMs: 1,
    parents: [otherSha],
    files: [
      { path: "src/readable.ts", status: "modified", insertions: 12, deletions: 4 },
      { path: "docs/typography.md", status: "added", insertions: 7, deletions: 0 },
    ],
  },
  {
    index: 1,
    sha: otherSha,
    message: "Single-file follow-up",
    committedAtMs: 2,
    parents: [sha],
    files: [{ path: "src/follow-up.ts", status: "modified", insertions: 3, deletions: 1 }],
  },
];

// Typed responses for development-only nonvisual query ports. Root supplies actual atoms.
export function diffFor(input: GitManagerGetDiffInput): GitManagerDiff {
  const metadata = { generation: 1, source: input.source, byteLength: 64, longestLineLength: 28 };
  if (input.cwd.endsWith("unrenderable")) return { ...metadata, _tag: "unrenderable" };
  if (input.cwd.endsWith("large"))
    return { ...metadata, _tag: "large-text", byteLength: 4_375_000 };
  return { ...metadata, _tag: "patch", patch: "component-fixture raw patch" };
}
export function remoteTagsFor(input: GitManagerRemoteTagsInput): GitManagerRemoteTags {
  return input.cwd.endsWith("unavailable")
    ? {
        remote: input.remote,
        status: "unavailable",
        reason: "Remote tags are unavailable in this fixture.",
        tags: [],
      }
    : {
        remote: input.remote,
        status: "available",
        reason: null,
        tags: [
          { name: "v0.8.2", targetSha: sha },
          { name: "v0.9.0", targetSha: otherSha },
        ],
      };
}
export function mergePreviewFor(input: GitManagerPreviewMergeInput): GitManagerMergePreview {
  return {
    _tag: "clean",
    source: input.source,
    current: input.target ?? "main",
    ahead: 3,
    behind: 2,
  };
}
export const refsSnapshot: GitManagerRefsSnapshot = {
  generation: 1,
  headRef: "feature/typography",
  detachedSha: null,
  isDirty: false,
  defaultBranch: "main",
  remotes: ["origin"],
  localBranches: refs,
  remoteBranches: remoteRefs,
  tags: [],
  worktrees: [],
  inProgressOperation: null,
  conflictedPaths: [],
};
export const commitPage: GitManagerCommitPage = {
  generation: 1,
  pinnedTips: [otherSha],
  commits: [{ ...commit, sha: otherSha, shortSha: "bbbbbbb" }],
  nextOffset: null,
  exhausted: true,
  degradedToAllPaging: false,
};
export const vcsStatus: VcsStatusResult = {
  isRepo: true,
  sourceControlProvider: { kind: "github", name: "GitHub", baseUrl: "https://github.com" },
  hasPrimaryRemote: true,
  isDefaultRef: false,
  refName: "feature/typography",
  defaultRefName: "main",
  hasWorkingTreeChanges: false,
  workingTree: { files: [], insertions: 0, deletions: 0 },
  hasUpstream: true,
  aheadCount: 3,
  behindCount: 0,
  pr: null,
};
export const vcsRefs: VcsListRefsResult = {
  isRepo: true,
  hasPrimaryRemote: true,
  nextCursor: null,
  totalCount: 3,
  refs: ["origin/main", "origin/release/next", "origin/feature/typography"].map((name) => ({
    name,
    isRemote: true,
    remoteName: "origin",
    current: false,
    isDefault: name === "origin/main",
    worktreePath: null,
  })),
};
export const createDefaults: PullRequestsCreateDefaults = {
  viewer: { id: "local-author", label: "Local Author" },
  squash: "default_off",
  removeSourceBranch: true,
};
export function vocabularyFor(input: PullRequestsVocabularyInput): PullRequestsVocabulary {
  const values: Record<
    PullRequestsVocabularyInput["kind"],
    readonly { id: string; label: string; color: string | null; description: string | null }[]
  > = {
    users: [
      { id: "local-author", label: "Local Author", color: null, description: null },
      { id: "local-reviewer", label: "Local Reviewer", color: null, description: null },
    ],
    labels: [
      {
        id: "readability",
        label: "readability",
        color: "267F61",
        description: "Readable secondary text",
      },
      {
        id: "typography",
        label: "typography",
        color: "64748B",
        description: "Text size and contrast",
      },
    ],
    milestones: [
      { id: "release-09", label: "Release 0.9", color: null, description: "Next release" },
      {
        id: "readability-pass",
        label: "Readability pass",
        color: null,
        description: "Typography acceptance",
      },
    ],
    branches: [
      { id: "main", label: "main", color: null, description: null },
      { id: "release/next", label: "release/next", color: null, description: null },
    ],
  };
  return { kind: input.kind, entries: values[input.kind], truncated: false };
}
