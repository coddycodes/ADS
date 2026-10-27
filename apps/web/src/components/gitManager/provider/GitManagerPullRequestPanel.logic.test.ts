import type {
  GitActionProgressEvent,
  GitManagerCommitEntry,
  GitRunStackedActionResult,
  SourceControlProviderInfo,
  SourceControlProviderKind,
  VcsStatusResult,
} from "@coddy/contracts";
import {
  NEUTRAL_CHANGE_REQUEST_PRESENTATION,
  resolveChangeRequestPresentation,
} from "@coddy/shared/sourceControl";
import { describe, expect, it } from "vite-plus/test";

import {
  createPullRequestAction,
  failCreatePullRequestProgress,
  presentCreatePullRequestProgress,
  reduceCreatePullRequestProgress,
  resolveCreatePullRequestReview,
  resolveProviderPanePresentation,
  resolveStatusChangeRequestPresentation,
  REVIEW_PROGRESS,
  UNIDENTIFIED_HOST_REASON,
  type CreatePullRequestProgress,
  createOptionsPayload,
  EMPTY_CREATE_OPTIONS,
  squashControl,
  shownCreateOptions,
  type CreateOptionsState,
} from "./GitManagerPullRequestPanel.logic";

function status(overrides: Partial<VcsStatusResult> = {}): VcsStatusResult {
  return {
    isRepo: true,
    sourceControlProvider: { kind: "github", name: "GitHub", baseUrl: "https://github.com" },
    hasPrimaryRemote: true,
    isDefaultRef: false,
    refName: "feature/reviewed",
    defaultRefName: "main",
    hasWorkingTreeChanges: false,
    workingTree: { files: [], insertions: 0, deletions: 0 },
    hasUpstream: false,
    aheadCount: 0,
    behindCount: 0,
    pr: null,
    ...overrides,
  } as VcsStatusResult;
}

function latestCommit(subject: string, body = ""): GitManagerCommitEntry {
  return {
    sha: "a".repeat(40),
    shortSha: "aaaaaaa",
    parents: [],
    decorations: [],
    subject,
    body,
    authorName: "Local",
    authorEmail: "local@example.test",
    authoredAtMs: 1,
    committerName: "Local",
    committerEmail: "local@example.test",
    committedAtMs: 1,
    changedFiles: [],
  };
}

const base = { actionId: "action-1", cwd: "/repo", action: "create_pr" } as const;

function finished(prStatus: "created" | "opened_existing"): GitActionProgressEvent {
  const result: GitRunStackedActionResult = {
    action: "create_pr",
    branch: { status: "skipped_not_requested" },
    commit: { status: "skipped_not_requested" },
    push: { status: "pushed", branch: "feature/reviewed" },
    pr: {
      status: prStatus,
      url: "https://github.com/owner/name/pull/7",
      number: 7,
      baseBranch: "main",
      headBranch: "feature/reviewed",
      title: "Reviewed",
    },
    toast: { title: "Git action completed", cta: { kind: "none" } },
  };
  return { ...base, kind: "action_finished", result };
}

function provider(kind: SourceControlProviderKind): SourceControlProviderInfo {
  return { kind, name: "Forge", baseUrl: "https://forge.invalid" };
}

describe("Git Manager provider pane logic", () => {
  it("starts not loaded and never implies an automatic request", () => {
    expect(
      resolveProviderPanePresentation({
        changeRequest: resolveStatusChangeRequestPresentation(null, true),
        requested: false,
        pending: false,
        error: null,
        result: null,
      }),
    ).toEqual({
      kind: "not-loaded",
      message: "Pull requests and checks load only when you choose Refresh.",
    });
  });

  it("renders provider unavailability as explanatory content", () => {
    expect(
      resolveProviderPanePresentation({
        changeRequest: resolveStatusChangeRequestPresentation(provider("github"), true),
        requested: true,
        pending: false,
        error: null,
        result: { status: "unavailable", pullRequests: [], checks: [] },
      }),
    ).toEqual({
      kind: "unavailable",
      message: "Pull requests or checks are unavailable for this repository provider.",
    });
  });

  it("keeps provider errors distinct from unavailable capability", () => {
    expect(
      resolveProviderPanePresentation({
        changeRequest: resolveStatusChangeRequestPresentation(provider("gitlab"), true),
        requested: true,
        pending: false,
        error: "Provider CLI failed.",
        result: null,
      }),
    ).toEqual({ kind: "error", message: "Provider CLI failed." });
  });

  it.each([
    ["GitHub", provider("github"), true, "pull request", "pull requests", "Pull requests"],
    ["GitLab", provider("gitlab"), true, "merge request", "merge requests", "Merge requests"],
    [
      "an unknown",
      provider("unknown"),
      true,
      "change request",
      "change requests",
      "Change requests",
    ],
    ["a missing", null, true, "pull request", "pull requests", "Pull requests"],
    ["a not-yet-named", null, false, "change request", "change requests", "Change requests"],
  ] as const)(
    "uses %s provider's vocabulary for each provider read state",
    (_label, host, statusLoaded, noun, plural, title) => {
      const input = {
        changeRequest: resolveStatusChangeRequestPresentation(host, statusLoaded),
        requested: true,
        pending: false,
        error: null,
        result: null,
      };
      expect(resolveProviderPanePresentation({ ...input, requested: false })).toEqual({
        kind: "not-loaded",
        message: `${title} and checks load only when you choose Refresh.`,
      });
      expect(resolveProviderPanePresentation({ ...input, pending: true })).toEqual({
        kind: "loading",
        message: `Loading ${plural} and checks…`,
      });
      expect(
        resolveProviderPanePresentation({
          ...input,
          result: { status: "unavailable", pullRequests: [], checks: [] },
        }),
      ).toEqual({
        kind: "unavailable",
        message: `${title} or checks are unavailable for this repository provider.`,
      });
      expect(
        resolveProviderPanePresentation({
          ...input,
          result: { status: "available", pullRequests: [], checks: [] },
        }),
      ).toEqual({
        kind: "loaded",
        message: `No open ${noun} was found for the current branch.`,
      });
    },
  );

  it("stays neutral until status answers, then follows the shared provider table", () => {
    // Nothing has named the host yet, so the shared neutral nouns apply.
    expect(resolveStatusChangeRequestPresentation(null, false)).toBe(
      NEUTRAL_CHANGE_REQUEST_PRESENTATION,
    );
    expect(resolveStatusChangeRequestPresentation(undefined, false)).toBe(
      NEUTRAL_CHANGE_REQUEST_PRESENTATION,
    );
    expect(NEUTRAL_CHANGE_REQUEST_PRESENTATION).toMatchObject({
      longName: "change request",
      pluralLongName: "change requests",
      numberPrefix: "#",
    });
    // Once status has answered, a missing provider takes the shared table's own rule.
    expect(resolveStatusChangeRequestPresentation(null, true)).toBe(
      resolveChangeRequestPresentation(undefined),
    );
    // A provider already known (reported, or a caller's hint) names its requests at once.
    expect(resolveStatusChangeRequestPresentation(provider("gitlab"), false)).toBe(
      resolveChangeRequestPresentation(provider("gitlab")),
    );
    expect(resolveStatusChangeRequestPresentation(provider("unknown"), true)).toBe(
      resolveChangeRequestPresentation(provider("unknown")),
    );
  });

  it("reuses the existing stacked create-pr action shape with the reviewed fields", () => {
    expect(
      createPullRequestAction("action-2", {
        title: "  Reviewed  ",
        body: "Body\n",
        baseBranch: "release/next",
      }),
    ).toEqual({
      actionId: "action-2",
      action: "create_pr",
      pullRequestTitle: "Reviewed",
      pullRequestBaseBranch: "release/next",
      pullRequestBody: "Body\n",
    });
    expect(
      createPullRequestAction("action-3", {
        title: "Reviewed",
        body: "   ",
        baseBranch: "release/next",
      }),
    ).toEqual({
      actionId: "action-3",
      action: "create_pr",
      pullRequestTitle: "Reviewed",
      pullRequestBaseBranch: "release/next",
    });
  });
});

describe("resolveCreatePullRequestReview", () => {
  it.each(["closed", "merged"] as const)(
    "allows a new request after the previous one was %s",
    (state) => {
      const review = resolveCreatePullRequestReview({
        status: status({
          pr: {
            state,
            number: 9,
            title: "Previous",
            url: "https://github.com/owner/repo/pull/9",
            baseRef: "main",
            headRef: "feature/reviewed",
          },
        }),
        latestCommit: null,
      });
      expect(review.existingPullRequest).toBeNull();
      expect(review.blockedReason).toBeNull();
    },
  );

  it("describes repository, branches, publish requirement, and commit-based defaults", () => {
    const review = resolveCreatePullRequestReview({
      status: status(),
      latestCommit: latestCommit("feat: reviewed change", "Longer body\n"),
    });

    expect(review).toEqual({
      provider: { kind: "github", name: "GitHub", baseUrl: "https://github.com" },
      head: "feature/reviewed",
      publishRequired: true,
      existingPullRequest: null,
      defaultTitle: "feat: reviewed change",
      defaultBody: "Longer body",
      blockedReason: null,
    });
  });

  it("requires publishing only when the upstream is missing or behind the local branch", () => {
    expect(
      resolveCreatePullRequestReview({
        status: status({ hasUpstream: true, aheadCount: 0 }),
        latestCommit: null,
      }).publishRequired,
    ).toBe(false);
    expect(
      resolveCreatePullRequestReview({
        status: status({ hasUpstream: true, aheadCount: 2 }),
        latestCommit: null,
      }).publishRequired,
    ).toBe(true);
  });

  it("falls back to a branch-named title and surfaces an existing pull request", () => {
    const review = resolveCreatePullRequestReview({
      status: status({
        pr: {
          number: 9,
          title: "Existing",
          url: "https://github.com/owner/name/pull/9",
          baseRef: "main",
          headRef: "feature/reviewed",
          state: "open",
        },
      }),
      latestCommit: null,
    });

    expect(review.defaultTitle).toBe("Update feature/reviewed");
    expect(review.existingPullRequest).toEqual({
      number: 9,
      title: "Existing",
      url: "https://github.com/owner/name/pull/9",
    });
  });

  it.each([
    [status({ isRepo: false }), "This folder is not a Git repository."],
    [status({ refName: null }), "Select a source branch."],
    [status({ sourceControlProvider: undefined }), UNIDENTIFIED_HOST_REASON],
    [
      status({ sourceControlProvider: undefined, hasPrimaryRemote: false }),
      "Add an origin remote to create a pull request.",
    ],
    [
      status({ hasWorkingTreeChanges: true }),
      "Commit local changes before creating a pull request.",
    ],
  ])("explains why creation is unavailable", (blockedStatus, reason) => {
    expect(
      resolveCreatePullRequestReview({ status: blockedStatus, latestCommit: null }).blockedReason,
    ).toBe(reason);
  });

  it("tells an unidentified host how to get identified", () => {
    expect(UNIDENTIFIED_HOST_REASON).toBe(
      "Coddy Studio hasn't identified this repository's host yet. Open Pull Requests for this project or run Rescan in Settings → Source Control.",
    );
  });

  it("uses the host's words: merge requests on GitLab, pull requests elsewhere", () => {
    const gitlab = status({
      sourceControlProvider: {
        kind: "gitlab",
        name: "GitLab",
        baseUrl: "https://luna.tripunkt.de",
      },
      hasWorkingTreeChanges: true,
    });
    expect(
      resolveCreatePullRequestReview({ status: gitlab, latestCommit: null }).blockedReason,
    ).toBe("Commit local changes before creating a merge request.");
    expect(
      resolveCreatePullRequestReview({
        status: status({ hasWorkingTreeChanges: true }),
        latestCommit: null,
      }).blockedReason,
    ).toBe("Commit local changes before creating a pull request.");
  });

  it("lets the Pull Requests panel's identified host stand in for a status without one", () => {
    const providerHint = { kind: "gitlab", baseUrl: "https://luna.tripunkt.de" } as const;
    const hinted = resolveCreatePullRequestReview({
      status: status({ sourceControlProvider: undefined }),
      latestCommit: null,
      providerHint,
    });
    expect(hinted.provider).toEqual({
      kind: "gitlab",
      name: "GitLab",
      baseUrl: "https://luna.tripunkt.de",
    });
    // The server validates the provider when creating; the review does not block.
    expect(hinted.blockedReason).toBeNull();
    // A status that names the host wins over the hint.
    expect(
      resolveCreatePullRequestReview({ status: status(), latestCommit: null, providerHint })
        .provider?.kind,
    ).toBe("github");
  });
});

describe("create pull request progress", () => {
  const review = { publishRequired: true, head: "feature/reviewed", provider: null };

  it("names a GitLab merge request with its reference in every phase", () => {
    const gitlab = {
      ...review,
      provider: { kind: "gitlab", name: "GitLab", baseUrl: "https://luna.tripunkt.de" } as const,
    };
    expect(presentCreatePullRequestProgress(REVIEW_PROGRESS, gitlab).primaryLabel).toBe(
      "Publish and create merge request",
    );
    expect(
      presentCreatePullRequestProgress(REVIEW_PROGRESS, { ...gitlab, publishRequired: false })
        .primaryLabel,
    ).toBe("Create merge request");
    expect(
      presentCreatePullRequestProgress({ kind: "running", phase: "pr", pushed: true }, gitlab)
        .status,
    ).toBe("Creating the merge request…");
    expect(
      presentCreatePullRequestProgress({ kind: "created", url: null, number: 14 }, gitlab).status,
    ).toBe("Merge request !14 created.");
    expect(
      presentCreatePullRequestProgress({ kind: "existing", url: null, number: 14 }, gitlab).status,
    ).toBe("Merge request !14 already exists for this branch, so none was created.");
    expect(
      presentCreatePullRequestProgress(
        { kind: "failed", phase: "pr", message: "denied", branchPublished: true },
        gitlab,
      ).status,
    ).toBe("feature/reviewed was published, but creating the merge request failed: denied");
  });

  function run(events: ReadonlyArray<GitActionProgressEvent>): CreatePullRequestProgress {
    return events.reduce(reduceCreatePullRequestProgress, REVIEW_PROGRESS);
  }

  it("tracks publish then create and settles on the created pull request", () => {
    const running = run([
      { ...base, kind: "action_started", phases: ["push", "pr"] },
      { ...base, kind: "phase_started", phase: "push", label: "Pushing" },
    ]);
    expect(presentCreatePullRequestProgress(running, review)).toMatchObject({
      status: "Publishing feature/reviewed…",
      busy: true,
    });

    const creating = reduceCreatePullRequestProgress(running, {
      ...base,
      kind: "phase_started",
      phase: "pr",
      label: "Creating pull request",
    });
    expect(creating).toEqual({ kind: "running", phase: "pr", pushed: true });
    expect(presentCreatePullRequestProgress(creating, review).status).toBe(
      "Creating the pull request…",
    );

    const created = reduceCreatePullRequestProgress(creating, finished("created"));
    expect(created).toEqual({
      kind: "created",
      url: "https://github.com/owner/name/pull/7",
      number: 7,
    });
    expect(presentCreatePullRequestProgress(created, review)).toMatchObject({
      status: "Pull request #7 created.",
      primaryLabel: "Done",
      settled: true,
    });
  });

  it("distinguishes a publish failure from a creation failure after the branch was published", () => {
    const publishFailed = run([
      { ...base, kind: "action_started", phases: ["push", "pr"] },
      { ...base, kind: "phase_started", phase: "push", label: "Pushing" },
      { ...base, kind: "action_failed", phase: "push", message: "remote rejected" },
    ]);
    expect(publishFailed).toEqual({
      kind: "failed",
      phase: "push",
      message: "remote rejected",
      branchPublished: false,
    });
    expect(presentCreatePullRequestProgress(publishFailed, review)).toMatchObject({
      status: "Publishing feature/reviewed failed: remote rejected",
      tone: "error",
      primaryLabel: "Retry",
    });

    const createFailed = run([
      { ...base, kind: "action_started", phases: ["push", "pr"] },
      { ...base, kind: "phase_started", phase: "push", label: "Pushing" },
      { ...base, kind: "phase_started", phase: "pr", label: "Creating pull request" },
      { ...base, kind: "action_failed", phase: "pr", message: "gh exited 1" },
    ]);
    expect(createFailed).toEqual({
      kind: "failed",
      phase: "pr",
      message: "gh exited 1",
      branchPublished: true,
    });
    expect(presentCreatePullRequestProgress(createFailed, review).status).toBe(
      "feature/reviewed was published, but creating the pull request failed: gh exited 1",
    );
  });

  it("reports an existing pull request without creating another", () => {
    const existing = run([
      { ...base, kind: "action_started", phases: ["push", "pr"] },
      { ...base, kind: "phase_started", phase: "pr", label: "Creating pull request" },
      finished("opened_existing"),
    ]);
    expect(existing).toEqual({
      kind: "existing",
      url: "https://github.com/owner/name/pull/7",
      number: 7,
    });
    expect(presentCreatePullRequestProgress(existing, review).status).toBe(
      "Pull request #7 already exists for this branch, so none was created.",
    );
  });

  it("settles a transport failure without losing a published branch or a settled outcome", () => {
    const creating = run([
      { ...base, kind: "action_started", phases: ["push", "pr"] },
      { ...base, kind: "phase_started", phase: "push", label: "Pushing" },
      { ...base, kind: "phase_started", phase: "pr", label: "Creating pull request" },
    ]);
    expect(failCreatePullRequestProgress(creating, "connection lost")).toEqual({
      kind: "failed",
      phase: "pr",
      message: "connection lost",
      branchPublished: true,
    });
    const created = reduceCreatePullRequestProgress(creating, finished("created"));
    expect(failCreatePullRequestProgress(created, "late failure")).toBe(created);
    const reported = reduceCreatePullRequestProgress(creating, {
      ...base,
      kind: "action_failed",
      phase: "pr",
      message: "gh exited 1",
    });
    expect(failCreatePullRequestProgress(reported, "transport lost")).toBe(reported);
  });
});

describe("resolveCreatePullRequestReview publication", () => {
  it("requires publication when only a differently named upstream exists", () => {
    const base = {
      status: status({ refName: "feature", hasUpstream: true, aheadCount: 0 }),
      latestCommit: null,
    };
    expect(resolveCreatePullRequestReview(base).publishRequired).toBe(false);
    expect(resolveCreatePullRequestReview({ ...base, headOnOrigin: false }).publishRequired).toBe(
      true,
    );
    expect(resolveCreatePullRequestReview({ ...base, headOnOrigin: true }).publishRequired).toBe(
      false,
    );
  });
});

describe("create options", () => {
  const chosen: CreateOptionsState = {
    ...EMPTY_CREATE_OPTIONS,
    draft: true,
    assignees: [{ id: "7", label: "alice" }],
    labels: [{ id: "bug", label: "bug" }],
    milestone: { id: "3", label: "Sprint 9" },
    removeSourceBranch: true,
    squash: false,
  };

  it("sends nothing when no option is set", () => {
    expect(createOptionsPayload(EMPTY_CREATE_OPTIONS, "gitlab")).toBeUndefined();
  });

  it("sends GitLab ids and merge options", () => {
    expect(createOptionsPayload(chosen, "gitlab")).toEqual({
      draft: true,
      assignees: ["7"],
      reviewers: [],
      labels: ["bug"],
      milestone: { id: "3", title: "Sprint 9" },
      removeSourceBranch: true,
      squash: false,
    });
  });

  it("never sends merge options for GitHub", () => {
    expect(createOptionsPayload(chosen, "github")).toMatchObject({
      removeSourceBranch: null,
      squash: null,
    });
  });

  it("sends nothing for providers without create options", () => {
    expect(createOptionsPayload(chosen, "azure-devops")).toBeUndefined();
    expect(createOptionsPayload(chosen, null)).toBeUndefined();
  });

  it("locks squash when the project forces it", () => {
    expect(squashControl({ squash: "always" })).toEqual({
      checked: true,
      locked: true,
      note: "This project always squashes commits.",
    });
    expect(squashControl({ squash: "never" })).toEqual({
      checked: false,
      locked: true,
      note: "This project never squashes commits.",
    });
    expect(squashControl({ squash: "default_on" })).toEqual({
      checked: true,
      locked: false,
      note: null,
    });
    expect(squashControl(null)).toEqual({ checked: false, locked: false, note: null });
  });

  it("adds options to the create action only when present", () => {
    const reviewed = { baseBranch: "main", title: "T", body: "" };
    expect(createPullRequestAction("a", reviewed)).not.toHaveProperty("pullRequestOptions");
    expect(
      createPullRequestAction("a", reviewed, createOptionsPayload(chosen, "gitlab")),
    ).toHaveProperty("pullRequestOptions.draft", true);
  });

  it("carries the server's partial-create warning into the outcome", () => {
    const event = finished("created");
    if (event.kind !== "action_finished") throw new Error("unexpected event");
    const warned = {
      ...event,
      result: { ...event.result, pr: { ...event.result.pr, warning: "Created, but…" } },
    };
    expect(reduceCreatePullRequestProgress(REVIEW_PROGRESS, warned)).toMatchObject({
      kind: "created",
      warning: "Created, but…",
    });
  });
});

describe("shownCreateOptions", () => {
  it("sends the merge options the dialog shows once the project defaults are known", () => {
    expect(
      shownCreateOptions(EMPTY_CREATE_OPTIONS, {
        viewer: null,
        squash: "default_on",
        removeSourceBranch: true,
      }),
    ).toMatchObject({ removeSourceBranch: true, squash: true });
    expect(
      shownCreateOptions(
        { ...EMPTY_CREATE_OPTIONS, squash: true },
        { viewer: null, squash: "never", removeSourceBranch: false },
      ),
    ).toMatchObject({ removeSourceBranch: false, squash: false });
  });

  it("leaves a merge option the project did not report to the project", () => {
    expect(
      shownCreateOptions(EMPTY_CREATE_OPTIONS, {
        viewer: null,
        squash: null,
        removeSourceBranch: null,
      }),
    ).toMatchObject({ removeSourceBranch: null, squash: null });
  });

  it("leaves untouched merge options unset without defaults", () => {
    expect(shownCreateOptions(EMPTY_CREATE_OPTIONS, null)).toEqual(EMPTY_CREATE_OPTIONS);
  });
});
