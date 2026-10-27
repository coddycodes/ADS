import type {
  GitActionProgressEvent,
  GitActionProgressPhase,
  GitManagerCommitEntry,
  GitManagerPullRequestsResult,
  GitPullRequestCreateOptions,
  GitRunStackedActionResult,
  PullRequestsCreateDefaults,
  PullRequestsSquashOption,
  PullRequestsProviderKind,
  SourceControlProviderInfo,
  VcsStatusResult,
} from "@coddy/contracts";
import {
  formatChangeRequestNumber,
  getChangeRequestTerminology,
  NEUTRAL_CHANGE_REQUEST_PRESENTATION,
  resolveChangeRequestPresentation,
  resolveChangeRequestPresentationForKind,
  type ChangeRequestPresentation,
} from "@coddy/shared/sourceControl";
import { capitalize } from "effect/String";

/**
 * Request nouns for a provider that status may not have reported yet. A known
 * provider (reported, or a caller's hint) names its own requests, and once status
 * has answered a missing provider follows the shared table's rule. Before status
 * answers (while it loads, or after it failed without an earlier answer), nothing
 * has named the host, so every Git Manager surface uses the shared neutral nouns
 * (decided 2026-09-26).
 */
export function resolveStatusChangeRequestPresentation(
  provider: SourceControlProviderInfo | null | undefined,
  statusLoaded: boolean,
): ChangeRequestPresentation {
  return provider == null && !statusLoaded
    ? NEUTRAL_CHANGE_REQUEST_PRESENTATION
    : resolveChangeRequestPresentation(provider);
}

export type ProviderPanePresentation =
  | { readonly kind: "not-loaded"; readonly message: string }
  | { readonly kind: "loading"; readonly message: string }
  | { readonly kind: "error"; readonly message: string }
  | { readonly kind: "unavailable"; readonly message: string }
  | { readonly kind: "loaded"; readonly message: string };

export function resolveProviderPanePresentation(input: {
  /** The request nouns the pane shows, from `resolveStatusChangeRequestPresentation`. */
  readonly changeRequest: ChangeRequestPresentation;
  readonly requested: boolean;
  readonly pending: boolean;
  readonly error: string | null;
  readonly result: GitManagerPullRequestsResult | null;
}): ProviderPanePresentation {
  const { changeRequest } = input;
  const pluralTitle = capitalize(changeRequest.pluralLongName);
  if (!input.requested) {
    return {
      kind: "not-loaded",
      message: `${pluralTitle} and checks load only when you choose Refresh.`,
    };
  }
  if (input.pending) {
    return { kind: "loading", message: `Loading ${changeRequest.pluralLongName} and checks…` };
  }
  if (input.error !== null) {
    return { kind: "error", message: input.error };
  }
  if (input.result?.status === "unavailable") {
    return {
      kind: "unavailable",
      message: `${pluralTitle} or checks are unavailable for this repository provider.`,
    };
  }
  return {
    kind: "loaded",
    message:
      input.result?.pullRequests.length === 0
        ? `No open ${changeRequest.longName} was found for the current branch.`
        : `${pluralTitle} and checks loaded.`,
  };
}

export interface ReviewedPullRequest {
  readonly title: string;
  readonly body: string;
  readonly baseBranch: string;
  readonly headBranch?: string;
}

/**
 * The stacked `create_pr` action carrying the reviewed branches, title and body. The body
 * is sent only when the user wrote one so the server keeps its empty default.
 */
export function createPullRequestAction(
  actionId: string,
  reviewed: ReviewedPullRequest,
  options?: GitPullRequestCreateOptions,
) {
  const title = reviewed.title.trim();
  const body = reviewed.body;
  return {
    actionId,
    action: "create_pr" as const,
    pullRequestBaseBranch: reviewed.baseBranch,
    ...(reviewed.headBranch === undefined ? {} : { pullRequestHeadBranch: reviewed.headBranch }),
    ...(title.length > 0 ? { pullRequestTitle: title } : {}),
    ...(body.trim().length > 0 ? { pullRequestBody: body } : {}),
    ...(options === undefined ? {} : { pullRequestOptions: options }),
  };
}

export interface VocabularyChoice {
  readonly id: string;
  readonly label: string;
}

export interface CreateOptionsState {
  readonly draft: boolean;
  readonly assignees: readonly VocabularyChoice[];
  readonly reviewers: readonly VocabularyChoice[];
  readonly labels: readonly VocabularyChoice[];
  readonly milestone: VocabularyChoice | null;
  /** `null` until the user changes it; the dialog then shows the project's default. */
  readonly removeSourceBranch: boolean | null;
  readonly squash: boolean | null;
}

export const EMPTY_CREATE_OPTIONS: CreateOptionsState = {
  draft: false,
  assignees: [],
  reviewers: [],
  labels: [],
  milestone: null,
  removeSourceBranch: null,
  squash: null,
};

/** Create options exist for GitHub and GitLab only; the server refuses them elsewhere. */
export function supportsCreateOptions(providerKind: string | null): boolean {
  return providerKind === "github" || providerKind === "gitlab";
}

/** The reviewed options, or `undefined` when nothing differs from the provider's defaults. */
export function createOptionsPayload(
  state: CreateOptionsState,
  providerKind: string | null,
): GitPullRequestCreateOptions | undefined {
  if (!supportsCreateOptions(providerKind)) return undefined;
  const gitlab = providerKind === "gitlab";
  const payload: GitPullRequestCreateOptions = {
    draft: state.draft,
    assignees: state.assignees.map((choice) => choice.id),
    reviewers: state.reviewers.map((choice) => choice.id),
    labels: state.labels.map((choice) => choice.id),
    milestone:
      state.milestone === null ? null : { id: state.milestone.id, title: state.milestone.label },
    removeSourceBranch: gitlab ? state.removeSourceBranch : null,
    squash: gitlab ? state.squash : null,
  };
  const empty =
    !payload.draft &&
    payload.assignees.length === 0 &&
    payload.reviewers.length === 0 &&
    payload.labels.length === 0 &&
    payload.milestone === null &&
    payload.removeSourceBranch === null &&
    payload.squash === null;
  return empty ? undefined : payload;
}

/**
 * The merge options exactly as the dialog displays them: once the project's defaults are known,
 * untouched boxes send their shown value, and a project-forced squash always wins.
 */
export function shownCreateOptions(
  state: CreateOptionsState,
  defaults: PullRequestsCreateDefaults | null,
): CreateOptionsState {
  if (defaults === null) return state;
  const squash = squashControl(defaults);
  return {
    ...state,
    removeSourceBranch: state.removeSourceBranch ?? defaults.removeSourceBranch,
    // An unreported project setting stays the project's decision until the user picks one.
    squash: squash.locked
      ? squash.checked
      : (state.squash ?? (defaults.squash === null ? null : squash.checked)),
  };
}

export function squashControl(
  defaults: { readonly squash: PullRequestsSquashOption | null } | null,
): { checked: boolean; locked: boolean; note: string | null } {
  switch (defaults?.squash) {
    case "always":
      return { checked: true, locked: true, note: "This project always squashes commits." };
    case "never":
      return { checked: false, locked: true, note: "This project never squashes commits." };
    case "default_on":
      return { checked: true, locked: false, note: null };
    default:
      return { checked: false, locked: false, note: null };
  }
}

export interface ExistingPullRequestSummary {
  readonly number: number;
  readonly title: string;
  readonly url: string;
}

/** A host the caller already identified: the Pull Requests panel's context. */
export interface CreatePullRequestProviderHint {
  readonly kind: PullRequestsProviderKind;
  /** The host's web address as the server reported it, never built here. */
  readonly baseUrl: string;
}

/** The hinted host as a provider, the way status names a GitHub or GitLab host. */
export function hintedProvider(
  hint: CreatePullRequestProviderHint | null,
): SourceControlProviderInfo | null {
  return hint === null
    ? null
    : {
        kind: hint.kind,
        name: resolveChangeRequestPresentationForKind(hint.kind).providerName,
        baseUrl: hint.baseUrl,
      };
}

export const UNIDENTIFIED_HOST_REASON =
  "Coddy Studio hasn't identified this repository's host yet. Open Pull Requests for this project or run Rescan in Settings → Source Control.";

/**
 * Everything the review surface shows before anything is published: where the
 * pull request goes, which branches it joins, whether the branch must be
 * published first, and why the action is unavailable when it is.
 */
export interface CreatePullRequestReview {
  readonly provider: SourceControlProviderInfo | null;
  readonly head: string | null;
  readonly publishRequired: boolean;
  readonly existingPullRequest: ExistingPullRequestSummary | null;
  readonly defaultTitle: string;
  readonly defaultBody: string;
  readonly blockedReason: string | null;
}

export function resolveCreatePullRequestReview(input: {
  readonly status: VcsStatusResult;
  readonly latestCommit: GitManagerCommitEntry | null;
  readonly providerHint?: CreatePullRequestProviderHint | null;
  readonly commitBeforeCreate?: boolean;
  readonly headBranch?: string | null;
  /** Whether origin already has a branch named like the source; undefined while unknown. */
  readonly headOnOrigin?: boolean;
}): CreatePullRequestReview {
  const { status, latestCommit } = input;
  // A cold or stale status may not name a host the caller already identified; the
  // hint stands in for it, and the server validates the provider when creating.
  const provider = status.sourceControlProvider ?? hintedProvider(input.providerHint ?? null);
  const noun = getChangeRequestTerminology(provider).singular;
  const head = input.headBranch === undefined ? status.refName : input.headBranch;
  const isCurrentBranch = head === status.refName;
  const existingPullRequest =
    !isCurrentBranch || status.pr === null || status.pr.state !== "open"
      ? null
      : { number: status.pr.number, title: status.pr.title, url: status.pr.url };
  const blockedReason = !status.isRepo
    ? "This folder is not a Git repository."
    : provider === null
      ? status.hasPrimaryRemote
        ? UNIDENTIFIED_HOST_REASON
        : `Add an origin remote to create a ${noun}.`
      : head === null
        ? "Select a source branch."
        : isCurrentBranch && status.hasWorkingTreeChanges && !input.commitBeforeCreate
          ? `Commit local changes before creating a ${noun}.`
          : null;
  const defaultTitle = latestCommit?.subject.trim() ?? "";
  return {
    provider,
    head,
    // An upstream with another name (`checkout -b feature origin/main`) does not
    // publish the source, so a missing same-named origin branch decides.
    publishRequired:
      !isCurrentBranch ||
      !status.hasUpstream ||
      status.aheadCount > 0 ||
      input.headOnOrigin === false,
    existingPullRequest,
    defaultTitle: defaultTitle.length > 0 ? defaultTitle : head === null ? "" : `Update ${head}`,
    defaultBody: latestCommit?.body.trim() ?? "",
    blockedReason,
  };
}

export type CreatePullRequestProgress =
  | { readonly kind: "review" }
  | {
      readonly kind: "running";
      readonly phase: GitActionProgressPhase | null;
      readonly pushed: boolean;
    }
  | {
      readonly kind: "failed";
      readonly phase: GitActionProgressPhase | null;
      readonly message: string;
      /** The branch was published in this attempt before the failure. */
      readonly branchPublished: boolean;
    }
  | {
      readonly kind: "created";
      readonly url: string | null;
      readonly number: number | null;
      readonly warning?: string;
    }
  | {
      readonly kind: "existing";
      readonly url: string | null;
      readonly number: number | null;
      readonly warning?: string;
    };

export const REVIEW_PROGRESS: CreatePullRequestProgress = { kind: "review" };

function pullRequestOutcome(result: GitRunStackedActionResult): CreatePullRequestProgress {
  const kind = result.pr.status === "opened_existing" ? "existing" : "created";
  return {
    kind,
    url: result.pr.url ?? null,
    number: result.pr.number ?? null,
    ...(result.pr.warning === undefined ? {} : { warning: result.pr.warning }),
  };
}

/** Folds server progress events into the dialog's progress state. */
export function reduceCreatePullRequestProgress(
  state: CreatePullRequestProgress,
  event: GitActionProgressEvent,
): CreatePullRequestProgress {
  const pushed = state.kind === "running" && (state.pushed || state.phase === "push");
  switch (event.kind) {
    case "action_started":
      return { kind: "running", phase: null, pushed: false };
    case "phase_started":
      return { kind: "running", phase: event.phase, pushed };
    case "action_finished":
      return pullRequestOutcome(event.result);
    case "action_failed":
      return {
        kind: "failed",
        phase: event.phase,
        message: event.message,
        branchPublished: pushed && event.phase === "pr",
      };
    case "hook_started":
    case "hook_output":
    case "hook_finished":
      return state.kind === "running" ? state : { kind: "running", phase: null, pushed };
  }
}

/** The failure state when the action settled without an `action_failed` event. */
export function failCreatePullRequestProgress(
  state: CreatePullRequestProgress,
  message: string,
): CreatePullRequestProgress {
  // A settled outcome or the server's own `action_failed` report is authoritative.
  if (state.kind === "created" || state.kind === "existing" || state.kind === "failed") {
    return state;
  }
  const phase = state.kind === "running" ? state.phase : null;
  const pushed = state.kind === "running" && (state.pushed || state.phase === "push");
  return {
    kind: "failed",
    phase,
    message,
    branchPublished: pushed && phase === "pr",
  };
}

export interface CreatePullRequestProgressPresentation {
  readonly status: string | null;
  readonly tone: "neutral" | "error" | "success";
  readonly primaryLabel: string;
  readonly busy: boolean;
  readonly settled: boolean;
}

export function presentCreatePullRequestProgress(
  state: CreatePullRequestProgress,
  review: Pick<CreatePullRequestReview, "publishRequired" | "head" | "provider">,
): CreatePullRequestProgressPresentation {
  const kind = review.provider?.kind ?? null;
  const noun = getChangeRequestTerminology(review.provider).singular;
  switch (state.kind) {
    case "review":
      return {
        status: null,
        tone: "neutral",
        primaryLabel: review.publishRequired ? `Publish and create ${noun}` : `Create ${noun}`,
        busy: false,
        settled: false,
      };
    case "running":
      return {
        status:
          state.phase === "push"
            ? `Publishing ${review.head ?? "the branch"}…`
            : state.phase === "pr"
              ? `Creating the ${noun}…`
              : "Starting…",
        tone: "neutral",
        primaryLabel: "Working…",
        busy: true,
        settled: false,
      };
    case "failed":
      return {
        status:
          state.phase === "push"
            ? `Publishing ${review.head ?? "the branch"} failed: ${state.message}`
            : state.branchPublished
              ? `${review.head ?? "The branch"} was published, but creating the ${noun} failed: ${state.message}`
              : `Creating the ${noun} failed: ${state.message}`,
        tone: "error",
        primaryLabel: "Retry",
        busy: false,
        settled: false,
      };
    case "created":
      return {
        status:
          state.number === null
            ? `${capitalize(noun)} created.`
            : `${capitalize(noun)} ${formatChangeRequestNumber(kind, state.number)} created.`,
        tone: "success",
        primaryLabel: "Done",
        busy: false,
        settled: true,
      };
    case "existing":
      return {
        status:
          state.number === null
            ? `A ${noun} already exists for this branch, so none was created.`
            : `${capitalize(noun)} ${formatChangeRequestNumber(kind, state.number)} already exists for this branch, so none was created.`,
        tone: "success",
        primaryLabel: "Done",
        busy: false,
        settled: true,
      };
  }
}
