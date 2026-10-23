import { describe, expect, it } from "vite-plus/test";
import * as Schema from "effect/Schema";

import {
  GitCancelCloneInput,
  GitCloneInput,
  GitCloneOperationError,
  GitCommandError,
  GitManagerError,
  GitManagerServiceError,
  VcsCreateWorktreeInput,
  VcsRemoveWorktreeInput,
  GitPreparePullRequestThreadInput,
  GitPullRequestMaterializationError,
  GitRunStackedActionInput,
  GitRunStackedActionResult,
  GitResolvePullRequestResult,
  TextGenerationError,
  VcsStatusLocalResult,
  VcsStatusResult,
  VcsStatusStreamEvent,
} from "./git.ts";
import { SourceControlProviderError } from "./sourceControl.ts";
import {
  expectDecodeFailure,
  expectEncodeFailure,
  makeInvalidClassInstance,
} from "./test/schemaAssertions.ts";

const decodeCreateWorktreeInput = Schema.decodeUnknownSync(VcsCreateWorktreeInput);
const decodeRemoveWorktreeInput = Schema.decodeUnknownSync(VcsRemoveWorktreeInput);
// The Rust server rejects unknown fields; Effect 4 parse options are per operation.
const decodePreparePullRequestThreadInput = Schema.decodeUnknownSync(
  GitPreparePullRequestThreadInput,
  { onExcessProperty: "error" },
);
const decodeRunStackedActionInput = Schema.decodeUnknownSync(GitRunStackedActionInput);
const decodeRunStackedActionResult = Schema.decodeUnknownSync(GitRunStackedActionResult);
const decodeCloneInput = Schema.decodeUnknownSync(GitCloneInput);
const decodeCloneOperationError = Schema.decodeUnknownSync(GitCloneOperationError);
const decodeCancelCloneInput = Schema.decodeUnknownSync(GitCancelCloneInput);
const decodeResolvePullRequestResult = Schema.decodeUnknownSync(GitResolvePullRequestResult);
const decodeManagerServiceError = Schema.decodeUnknownSync(GitManagerServiceError);
const encodeManagerServiceError = Schema.encodeUnknownSync(GitManagerServiceError);
const decodeStatusLocalResult = Schema.decodeUnknownSync(VcsStatusLocalResult);
const encodeStatusLocalResult = Schema.encodeSync(VcsStatusLocalResult);
const decodeStatusResult = Schema.decodeUnknownSync(VcsStatusResult);
const encodeStatusResult = Schema.encodeSync(VcsStatusResult);
const decodeStatusStreamEvent = Schema.decodeUnknownSync(VcsStatusStreamEvent);
const encodeStatusStreamEvent = Schema.encodeSync(VcsStatusStreamEvent);

describe("VCS repository availability", () => {
  const local = {
    isRepo: false,
    hasPrimaryRemote: false,
    isDefaultRef: false,
    refName: null,
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
  };
  const remote = { hasUpstream: false, aheadCount: 0, behindCount: 0, pr: null };

  it.each(["absent", "unreadable", "untrusted"])(
    "round-trips %s across local status shapes",
    (reason) => {
      const unavailable = { ...local, repositoryUnavailableReason: reason };
      expect(encodeStatusLocalResult(decodeStatusLocalResult(unavailable))).toEqual(unavailable);
      expect(encodeStatusResult(decodeStatusResult({ ...unavailable, ...remote }))).toEqual({
        ...unavailable,
        ...remote,
      });
      for (const event of [
        { _tag: "snapshot", local: unavailable, remote: null },
        { _tag: "localUpdated", local: unavailable },
      ]) {
        expect(encodeStatusStreamEvent(decodeStatusStreamEvent(event))).toEqual(event);
      }
    },
  );

  it("accepts older servers and healthy repositories without a reason", () => {
    for (const isRepo of [false, true]) {
      const status = { ...local, isRepo };
      expect(decodeStatusLocalResult(status)).toEqual(status);
      expect(decodeStatusResult({ ...status, ...remote })).toEqual({
        ...status,
        ...remote,
      });
    }
  });

  it("decodes an unknown repository reason in local status as absent", () => {
    const decoded = decodeStatusLocalResult({
      ...local,
      repositoryUnavailableReason: "futureReason",
    });

    expect(decoded).toStrictEqual(local);
    expect(Object.hasOwn(decoded, "repositoryUnavailableReason")).toBe(false);
  });

  it("decodes an unknown repository reason in full status as absent", () => {
    const decoded = decodeStatusResult({
      ...local,
      ...remote,
      repositoryUnavailableReason: "futureReason",
    });

    expect(decoded).toStrictEqual({ ...local, ...remote });
    expect(Object.hasOwn(decoded, "repositoryUnavailableReason")).toBe(false);
  });

  it.each([
    { _tag: "snapshot", local, remote },
    { _tag: "localUpdated", local },
  ] as const)("decodes an unknown repository reason in $_tag as absent", (event) => {
    const decoded = decodeStatusStreamEvent({
      ...event,
      local: { ...local, repositoryUnavailableReason: "futureReason" },
    });

    expect(decoded).toStrictEqual(event);
    if (decoded._tag === "remoteUpdated") throw new Error("Expected a local status event");
    expect(Object.hasOwn(decoded.local, "repositoryUnavailableReason")).toBe(false);
  });
});

describe("VcsCreateWorktreeInput", () => {
  it("accepts omitted newRefName for existing-refName worktrees", () => {
    const parsed = decodeCreateWorktreeInput({
      cwd: "/repo",
      refName: "feature/existing",
      path: "/tmp/worktree",
    });

    expect(parsed.newRefName).toBeUndefined();
    expect(parsed.refName).toBe("feature/existing");
  });

  it("accepts explicit null refs for existing-ref worktrees", () => {
    const parsed = decodeCreateWorktreeInput({
      cwd: "/repo",
      refName: "feature/existing",
      newRefName: null,
      baseRefName: null,
      path: null,
    });

    expect(parsed.newRefName).toBeNull();
    expect(parsed.baseRefName).toBeNull();
  });

  it("accepts baseRefName metadata for a new worktree ref", () => {
    const parsed = decodeCreateWorktreeInput({
      cwd: "/repo",
      refName: "0123456789abcdef",
      newRefName: "feature/new",
      baseRefName: "origin/main",
      path: "/tmp/worktree",
    });

    expect(parsed.baseRefName).toBe("origin/main");
  });
});

describe("VcsRemoveWorktreeInput", () => {
  it("requires every terminal-owning thread to be fenced by the server", () => {
    const parsed = decodeRemoveWorktreeInput({
      cwd: "/repo",
      path: "/repo-worktrees/feature",
      force: true,
      ownerThreadId: "workspace-thread",
      threadIds: ["workspace-thread", "panel-thread"],
    });

    expect(parsed.ownerThreadId).toBe("workspace-thread");
    expect(parsed.threadIds).toEqual(["workspace-thread", "panel-thread"]);
    expect(() =>
      decodeRemoveWorktreeInput({
        cwd: "/repo",
        path: "/repo-worktrees/feature",
        force: true,
      }),
    ).toThrow();
    expect(() =>
      decodeRemoveWorktreeInput({
        cwd: "/repo",
        path: "/repo-worktrees/feature",
        force: true,
        ownerThreadId: "workspace-thread",
        threadIds: [],
      }),
    ).toThrow();
  });
});

describe("GitPreparePullRequestThreadInput", () => {
  it("accepts only local preparation without client-directed owner identity", () => {
    const parsed = decodePreparePullRequestThreadInput({
      cwd: "/repo",
      reference: "#42",
      mode: "local",
    });

    expect(parsed.reference).toBe("#42");
    expect(parsed.mode).toBe("local");
    expect(() =>
      decodePreparePullRequestThreadInput({
        cwd: "/repo",
        reference: "#42",
        mode: "worktree",
      }),
    ).toThrow();
    expect(() =>
      decodePreparePullRequestThreadInput({
        cwd: "/repo",
        reference: "#42",
        mode: "local",
        threadId: "caller-selected-owner",
      }),
    ).toThrow();
  });
});

describe("GitResolvePullRequestResult", () => {
  it("decodes resolved pull request metadata", () => {
    const parsed = decodeResolvePullRequestResult({
      pullRequest: {
        number: 42,
        title: "PR threads",
        url: "https://github.com/mubeda/BibCode/pull/42",
        baseBranch: "main",
        headBranch: "feature/pr-threads",
        state: "open",
      },
    });

    expect(parsed.pullRequest.number).toBe(42);
    expect(parsed.pullRequest.headBranch).toBe("feature/pr-threads");
  });
});

describe("GitRunStackedActionInput", () => {
  it("accepts explicit stacked actions and requires a client-provided actionId", () => {
    const parsed = decodeRunStackedActionInput({
      actionId: "action-1",
      cwd: "/repo",
      action: "create_pr",
    });

    expect(parsed.actionId).toBe("action-1");
    expect(parsed.action).toBe("create_pr");
  });

  it("carries a reviewed pull-request target, title and body and rejects blank fields", () => {
    const parsed = decodeRunStackedActionInput({
      actionId: "action-2",
      cwd: "/repo",
      action: "create_pr",
      pullRequestTitle: "  Reviewed title  ",
      pullRequestBody: "",
      pullRequestBaseBranch: "release/next",
      pullRequestHeadBranch: "feature/other",
    });

    expect(parsed.pullRequestTitle).toBe("Reviewed title");
    expect(parsed.pullRequestBody).toBe("");
    expect(parsed.pullRequestBaseBranch).toBe("release/next");
    expect(parsed.pullRequestHeadBranch).toBe("feature/other");
    expect(() =>
      decodeRunStackedActionInput({
        actionId: "blank-source",
        cwd: "/repo",
        action: "create_pr",
        pullRequestHeadBranch: " ",
      }),
    ).toThrow();
    expect(() =>
      decodeRunStackedActionInput({
        actionId: "blank-target",
        cwd: "/repo",
        action: "create_pr",
        pullRequestBaseBranch: " ",
      }),
    ).toThrow();
    expect(() =>
      decodeRunStackedActionInput({
        actionId: "action-3",
        cwd: "/repo",
        action: "create_pr",
        pullRequestTitle: "   ",
      }),
    ).toThrow();
  });
});

describe("GitRunStackedActionResult", () => {
  it("decodes a server-authored completion toast", () => {
    const parsed = decodeRunStackedActionResult({
      action: "commit_push",
      branch: {
        status: "created",
        name: "feature/server-owned-toast",
      },
      commit: {
        status: "created",
        commitSha: "89abcdef01234567",
        subject: "feat: move toast state into git manager",
      },
      push: {
        status: "pushed",
        branch: "feature/server-owned-toast",
        upstreamBranch: "origin/feature/server-owned-toast",
      },
      pr: {
        status: "skipped_not_requested",
      },
      toast: {
        title: "Pushed 89abcde to origin/feature/server-owned-toast",
        description: "feat: move toast state into git manager",
        cta: {
          kind: "run_action",
          label: "Create PR",
          action: {
            kind: "create_pr",
          },
        },
      },
    });

    expect(parsed.toast.cta.kind).toBe("run_action");
    if (parsed.toast.cta.kind === "run_action") {
      expect(parsed.toast.cta.action.kind).toBe("create_pr");
    }
  });
});

describe("git errors", () => {
  const errors = [
    new GitCommandError({
      operation: "status",
      command: "git status --short",
      cwd: "/repo",
      argumentCount: 2,
      exitCode: 128,
      stderrLength: 20,
      detail: "not a repository",
    }),
    new TextGenerationError({
      operation: "commit message",
      detail: "model unavailable",
    }),
    new GitManagerError({
      operation: "refresh",
      cwd: "/repo",
      detail: "repository state unavailable",
      cause: "io failure",
    }),
    new GitPullRequestMaterializationError({
      cwd: "/repo",
      pullRequestNumber: 42,
      headRepository: null,
      headBranch: "feature/pr",
      localBranch: "codex/pr-42",
      cause: "fetch failed",
    }),
  ] as const;

  it("constructs every git tagged error and preserves optional diagnostics", () => {
    expect(errors.map((error) => error.message)).toEqual([
      "Git command failed in status (/repo): not a repository",
      "Text generation failed in commit message: model unavailable",
      "Git manager failed in refresh: repository state unavailable",
      "Failed to materialize pull request #42 branch feature/pr as codex/pr-42.",
    ]);
    expect(errors[0].argumentCount).toBe(2);
    expect(errors[0].outputLength).toBeUndefined();
    expect(errors[1].cause).toBeUndefined();
  });

  it("round-trips every manager service error alternative", () => {
    const sourceControlError = new SourceControlProviderError({
      provider: "github",
      operation: "resolve",
      cwd: "/repo",
      detail: "not found",
    });
    for (const error of [...errors, sourceControlError]) {
      const encoded = encodeManagerServiceError(error);
      const decoded = decodeManagerServiceError(encoded);
      expect(decoded._tag).toBe(error._tag);
    }
  });

  it("reports invalid pull request numbers on decode and encode", () => {
    const invalid = {
      _tag: "GitPullRequestMaterializationError",
      cwd: "/repo",
      pullRequestNumber: 0,
      headRepository: null,
      headBranch: "feature/pr",
      localBranch: "codex/pr-0",
      cause: "fetch failed",
    };
    const expectedPath = {
      paths: [["pullRequestNumber"]],
      containsTag: "InvalidValue" as const,
    };
    const decodeExpected = {
      ...expectedPath,
      rootTag: "Encoding" as const,
    };
    const encodeExpected = { ...expectedPath, rootTag: "Composite" as const };
    expectDecodeFailure(GitPullRequestMaterializationError, invalid, decodeExpected);
    expectEncodeFailure(
      GitPullRequestMaterializationError,
      makeInvalidClassInstance(GitPullRequestMaterializationError.prototype, invalid),
      encodeExpected,
    );
  });
});

describe("clone re-attach contracts", () => {
  it("keeps attach and detach optional on the clone input", () => {
    expect(decodeCloneInput({ url: "https://example.test/demo.git", parentDir: "/code" })).toEqual({
      url: "https://example.test/demo.git",
      parentDir: "/code",
    });
    expect(
      decodeCloneInput({
        url: "https://example.test/demo.git",
        parentDir: "/code",
        directoryName: "demo",
        attach: true,
        detach: true,
      }),
    ).toMatchObject({ attach: true, detach: true, directoryName: "demo" });
  });

  it("decodes every clone operation reason and rejects unknown ones", () => {
    for (const reason of ["busy", "capacity", "shutting-down", "not-in-progress", "cancelled"]) {
      const error = decodeCloneOperationError({
        _tag: "GitCloneOperationError",
        reason,
        destination: "/code/demo",
        message: "Synthetic server message.",
      });
      expect(error.reason).toBe(reason);
      expect(error.message).toBe("Synthetic server message.");
    }
    expect(() =>
      decodeCloneOperationError({
        _tag: "GitCloneOperationError",
        reason: "unknown",
        destination: "/code/demo",
        message: "x",
      }),
    ).toThrow();
  });

  it("cancels by the clone's own input so only the server derives the destination", () => {
    expect(
      decodeCancelCloneInput({ url: "https://example.test/demo.git", parentDir: "~/code" }),
    ).toEqual({ url: "https://example.test/demo.git", parentDir: "~/code" });
  });
});

describe("GitRunStackedActionInput pullRequestOptions", () => {
  const base = { actionId: "a", cwd: "/repo", action: "create_pr", pullRequestBaseBranch: "main" };
  const options = {
    draft: true,
    assignees: ["7"],
    reviewers: [],
    labels: ["bug"],
    milestone: { id: "3", title: "Sprint 9" },
    removeSourceBranch: true,
    squash: null,
  };
  it("accepts reviewed options", () => {
    expect(
      decodeRunStackedActionInput({ ...base, pullRequestOptions: options }).pullRequestOptions,
    ).toEqual(options);
  });
  it("caps assignees at 20 and labels at 50", () => {
    const ids = (n: number) => Array.from({ length: n }, (_, i) => `u${i}`);
    expect(() =>
      decodeRunStackedActionInput({
        ...base,
        pullRequestOptions: { ...options, assignees: ids(21) },
      }),
    ).toThrow();
    expect(() =>
      decodeRunStackedActionInput({ ...base, pullRequestOptions: { ...options, labels: ids(51) } }),
    ).toThrow();
  });
  it("reports a partial-create warning on the pull request step", () => {
    const result = decodeRunStackedActionResult({
      action: "create_pr",
      branch: { status: "skipped_not_requested" },
      commit: { status: "skipped_not_requested" },
      push: { status: "skipped_not_requested" },
      pr: {
        status: "created",
        url: "https://x/1",
        warning: "Created, but some options weren't applied.",
      },
      toast: { title: "Done", cta: { kind: "none" } },
    });
    expect(result.pr.warning).toBe("Created, but some options weren't applied.");
  });
});

describe("VCS conflict state", () => {
  const healthy = {
    isRepo: true,
    hasPrimaryRemote: false,
    isDefaultRef: false,
    refName: "main",
    hasWorkingTreeChanges: true,
    workingTree: {
      files: [
        { path: "a.txt", insertions: 0, deletions: 0, status: "conflicted", area: "unstaged" },
      ],
      insertions: 0,
      deletions: 0,
    },
  };

  it("decodes a conflicted file and an in-progress merge", () => {
    const local = { ...healthy, operationInProgress: "merge" };
    expect(encodeStatusLocalResult(decodeStatusLocalResult(local))).toEqual(local);
    const event = { _tag: "localUpdated", local };
    expect(encodeStatusStreamEvent(decodeStatusStreamEvent(event))).toEqual(event);
  });

  it("decodes an unknown future file status and operation as absent", () => {
    const decoded = decodeStatusLocalResult({
      ...healthy,
      operationInProgress: "future-operation",
      workingTree: {
        ...healthy.workingTree,
        files: [{ path: "a.txt", insertions: 0, deletions: 0, status: "future", area: "unstaged" }],
      },
    });
    expect(Object.hasOwn(decoded, "operationInProgress")).toBe(false);
    expect(Object.hasOwn(decoded.workingTree.files[0]!, "status")).toBe(false);
  });
});
