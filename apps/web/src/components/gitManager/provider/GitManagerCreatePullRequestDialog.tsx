import type {
  EnvironmentId,
  GitActionProgressEvent,
  GitManagerCommitEntry,
  GitRunStackedActionInput,
  GitRunStackedActionResult,
  VcsStatusResult,
} from "@bibcode/contracts";
import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import { useDebouncedValue } from "@tanstack/react-pacer";
import { AsyncResult } from "effect/unstable/reactivity";
import { GitPullRequestIcon, TriangleAlertIcon } from "lucide-react";
import {
  memo,
  type ChangeEvent,
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
} from "react";

import { Alert, AlertDescription, AlertTitle } from "~/components/ui/alert";
import { Button } from "~/components/ui/button";
import { Checkbox } from "~/components/ui/checkbox";
import {
  Combobox,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxPopup,
} from "~/components/ui/combobox";
import { PermissionButton } from "~/components/ui/permission-button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";
import { Input } from "~/components/ui/input";
import { Label } from "~/components/ui/label";
import { Textarea } from "~/components/ui/textarea";
import {
  formatChangeRequestNumber,
  resolveChangeRequestPresentation,
} from "@bibcode/shared/sourceControl";
import { capitalize } from "effect/String";
import { randomUUID } from "~/lib/utils";
import { gitManagerEnvironment } from "~/state/gitManager";
import { pullRequestsEnvironment } from "~/state/pullRequests";
import { useServerConfigs } from "~/state/entities";
import { useEnvironmentQuery } from "~/state/query";
import { useGitStackedAction } from "~/state/sourceControlActions";
import { vcsEnvironment } from "~/state/vcs";
import { useAtomCommand } from "~/state/use-atom-command";

import { usePullRequestsQuery } from "../../pullRequests/shared/usePullRequestsQuery";
import { CreatePullRequestOptions } from "./CreatePullRequestOptions";
import {
  createOptionsPayload,
  createPullRequestAction,
  EMPTY_CREATE_OPTIONS,
  failCreatePullRequestProgress,
  shownCreateOptions,
  supportsCreateOptions,
  type CreateOptionsState,
  hintedProvider,
  presentCreatePullRequestProgress,
  reduceCreatePullRequestProgress,
  resolveCreatePullRequestReview,
  resolveStatusChangeRequestPresentation,
  REVIEW_PROGRESS,
  type CreatePullRequestProgress,
  type CreatePullRequestProviderHint,
} from "./GitManagerPullRequestPanel.logic";

function safeExternalUrl(value: string | null): string | null {
  if (value === null) return null;
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:" ? url.href : null;
  } catch {
    return null;
  }
}

function failureMessage(error: unknown, noun: string): string {
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : `The ${noun} could not be created.`;
}

export interface GitManagerCreatePullRequestDialogProps {
  readonly open: boolean;
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly onOpenChange: (open: boolean) => void;
  /** Called once a pull request was created or found so the pane can refresh. */
  readonly onSettled: (result: GitRunStackedActionResult) => void;
  /** Legacy combined actions review the request before committing and publishing. */
  readonly commitInput?: Pick<
    GitRunStackedActionInput,
    "commitMessage" | "featureBranch" | "filePaths" | "commitStagedIndexAsIs"
  >;
  /**
   * A host the caller already identified (the Pull Requests panel). It stands in for a
   * status that has not named the host yet; the server validates it when creating.
   */
  readonly providerHint?: CreatePullRequestProviderHint | null;
}

/**
 * The review surface in front of `create_pr`. Opening it reads local status
 * only; nothing is published or created until the primary action is chosen.
 */
export const GitManagerCreatePullRequestDialog = memo(function GitManagerCreatePullRequestDialog(
  props: GitManagerCreatePullRequestDialogProps,
) {
  return props.open ? (
    <CreatePullRequestReviewDialog
      key={JSON.stringify([props.scope.environmentId, props.scope.cwd])}
      {...props}
    />
  ) : null;
});

function CreatePullRequestReviewDialog({
  open,
  scope,
  onOpenChange,
  onSettled,
  providerHint = null,
  commitInput,
}: GitManagerCreatePullRequestDialogProps) {
  const { environmentId, cwd } = scope;
  const serverConfig = useServerConfigs().get(environmentId) ?? null;
  const capabilityBlockedReason =
    serverConfig === null
      ? "Checking server support…"
      : serverConfig.environment.capabilities.gitPullRequestBranchSelection === true
        ? null
        : "Update this environment's BiBCode server to select source and target branches.";
  const [baseBranch, setBaseBranch] = useState<string | null>(null);
  const [headBranch, setHeadBranch] = useState<string | null | undefined>(undefined);
  const [headSearch, setHeadSearch] = useState<string | undefined>(undefined);
  const selectedHead = useRef<string | null>(null);
  const [branchSearch, setBranchSearch] = useState("");
  const statusAtom = useMemo(
    () => (open ? vcsEnvironment.status({ environmentId, input: { cwd } }) : null),
    [cwd, environmentId, open],
  );
  const statusQuery = useEnvironmentQuery(statusAtom);
  const refreshStatusQuery = statusQuery.refresh;
  const status: VcsStatusResult | null = statusQuery.data ?? null;
  const snapshotAtom = useMemo(
    () =>
      capabilityBlockedReason === null
        ? gitManagerEnvironment.getRefs({ environmentId, input: { cwd } })
        : null,
    [capabilityBlockedReason, cwd, environmentId],
  );
  const snapshot = useEnvironmentQuery(snapshotAtom).data;
  const targetIsDefault = baseBranch !== null && baseBranch === snapshot?.defaultBranch;
  const criticalTarget =
    targetIsDefault || baseBranch === "main" || baseBranch === "master" ? baseBranch : null;
  // The checkout's branch is the source by default; the server publishes it
  // first when origin does not have it yet.
  const defaultSource = status?.refName ?? null;
  const sourceBranch = headBranch === undefined ? defaultSource : headBranch;
  const sourceTip =
    snapshot?.localBranches.find((branch) => branch.name === sourceBranch)?.tipSha ??
    snapshot?.remoteBranches.find((branch) => branch.name === `origin/${sourceBranch}`)?.tipSha;
  const latestCommitAtom = useMemo(
    () =>
      sourceTip
        ? gitManagerEnvironment.getCommits({
            environmentId,
            input: { cwd, pinnedTips: [sourceTip], offset: 0, limit: 1 },
          })
        : null,
    [cwd, environmentId, sourceTip],
  );
  const latestCommitQuery = useEnvironmentQuery(latestCommitAtom);
  const candidateCommit = latestCommitQuery.data?.commits[0];
  const latestCommit: GitManagerCommitEntry | null =
    candidateCommit?.sha === sourceTip ? (candidateCommit ?? null) : null;
  const review = useMemo(
    () =>
      status === null
        ? null
        : resolveCreatePullRequestReview({
            status,
            latestCommit,
            providerHint,
            commitBeforeCreate: commitInput !== undefined,
            headBranch: sourceBranch,
            ...(snapshot == null
              ? {}
              : {
                  headOnOrigin: snapshot.remoteBranches.some(
                    (branch) => branch.name === `origin/${sourceBranch}`,
                  ),
                }),
          }),
    [commitInput, sourceBranch, latestCommit, providerHint, snapshot, status],
  );
  const provider = review === null ? hintedProvider(providerHint) : review.provider;
  // Until status answers, only a caller's hint names the host; without one stay neutral.
  const noun = resolveStatusChangeRequestPresentation(provider, review !== null).longName;
  const waitReason = `Wait for the ${noun} to finish.`;
  const providerKind = provider?.kind ?? null;
  const createOptionsSupported =
    serverConfig?.environment.capabilities.pullRequestCreateOptions === true &&
    supportsCreateOptions(providerKind);
  const [createOptions, setCreateOptions] = useState<CreateOptionsState>(EMPTY_CREATE_OPTIONS);
  const createDefaultsAtom = useMemo(
    () =>
      open && createOptionsSupported
        ? pullRequestsEnvironment.getCreateDefaults({ environmentId, input: { cwd } })
        : null,
    [createOptionsSupported, cwd, environmentId, open],
  );
  // An open re-reads cached defaults and hides them meanwhile: account, origin or project
  // settings can change at the same checkout path.
  const createDefaultsQuery = usePullRequestsQuery(createDefaultsAtom);
  const createDefaults = createDefaultsQuery.data ?? null;
  const createDefaultsState =
    createDefaults !== null ? "ready" : createDefaultsQuery.error ? "error" : "loading";

  const [editedTitle, setTitle] = useState<string>();
  const [editedBody, setBody] = useState<string>();
  const title = editedTitle ?? review?.defaultTitle ?? "";
  const body = editedBody ?? review?.defaultBody ?? "";
  const [progress, setProgress] = useState<CreatePullRequestProgress>(REVIEW_PROGRESS);
  const running = progress.kind === "running";
  // The outcome, its link and any warning sit below the form; bring them into view on settle.
  const statusRef = useRef<HTMLParagraphElement>(null);
  useEffect(() => {
    if (progress.kind === "created" || progress.kind === "existing" || progress.kind === "failed") {
      statusRef.current?.scrollIntoView?.({ block: "nearest" });
    }
  }, [progress.kind]);
  const [previousHead, setPreviousHead] = useState(review?.head);
  if (previousHead !== review?.head) {
    setPreviousHead(review?.head);
    if (progress.kind === "review" || progress.kind === "failed") {
      setBaseBranch(null);
      setBranchSearch("");
    }
  }

  const stackedAction = useGitStackedAction(scope);
  const runStackedAction = stackedAction.run;
  const refreshStatus = useAtomCommand(vcsEnvironment.refreshStatus, { reportFailure: false });
  const attemptedHead = useRef<string | null>(null);
  const presentation =
    review === null
      ? null
      : presentCreatePullRequestProgress(progress, {
          publishRequired: review.publishRequired,
          head: review.head,
          provider: review.provider,
        });
  const busy = presentation?.busy === true;
  const settled = presentation?.settled === true;
  const trimmedTitle = title.trim();
  const blockedReason = capabilityBlockedReason ?? review?.blockedReason ?? null;
  const primaryDisabledReason =
    review === null
      ? "Reading repository status…"
      : busy
        ? waitReason
        : settled
          ? null
          : (blockedReason ??
            (review.existingPullRequest !== null
              ? `A ${noun} already exists for this branch.`
              : trimmedTitle.length === 0
                ? `Enter a title for the ${noun}.`
                : baseBranch === null
                  ? "Select a target branch."
                  : !commitInput?.featureBranch && baseBranch === review.head
                    ? "Select a target branch different from the source branch."
                    : null));

  const onProgress = useCallback((event: GitActionProgressEvent) => {
    setProgress((current) => reduceCreatePullRequestProgress(current, event));
  }, []);

  const submit = useCallback(async () => {
    if (settled) {
      onOpenChange(false);
      return;
    }
    if (primaryDisabledReason !== null || baseBranch === null) return;
    const retrying = progress.kind === "failed";
    setProgress({ kind: "running", phase: null, pushed: false });
    let remainingCommit = commitInput;
    if (retrying && commitInput !== undefined) {
      // Native failures can arrive without a phase. Read Git before resuming
      // so a completed feature branch/commit is not attempted a second time.
      const refreshed = await refreshStatus({ environmentId, input: { cwd } });
      if (!AsyncResult.isSuccess(refreshed)) {
        setProgress((current) =>
          failCreatePullRequestProgress(
            current,
            failureMessage(squashAtomCommandFailure(refreshed), noun),
          ),
        );
        return;
      }
      if (refreshed.value.refName !== selectedHead.current) {
        setBaseBranch(null);
        setBranchSearch("");
        setProgress({
          kind: "failed",
          phase: null,
          branchPublished: false,
          message:
            "The source branch changed. Select a target branch to review the current branch before retrying.",
        });
        refreshStatusQuery();
        return;
      }
      if (!refreshed.value.hasWorkingTreeChanges) remainingCommit = undefined;
      else if (refreshed.value.refName !== attemptedHead.current)
        remainingCommit = { ...commitInput, featureBranch: false };
    } else {
      attemptedHead.current = sourceBranch;
    }
    const options = createOptionsSupported
      ? createOptionsPayload(shownCreateOptions(createOptions, createDefaults), providerKind)
      : undefined;
    const result = await runStackedAction({
      ...createPullRequestAction(
        randomUUID(),
        {
          title: trimmedTitle,
          body,
          baseBranch,
          ...(!remainingCommit?.featureBranch && sourceBranch ? { headBranch: sourceBranch } : {}),
        },
        options,
      ),
      ...(remainingCommit?.commitMessage ? { commitMessage: remainingCommit.commitMessage } : {}),
      ...(remainingCommit?.featureBranch ? { featureBranch: true } : {}),
      ...(remainingCommit?.filePaths ? { filePaths: [...remainingCommit.filePaths] } : {}),
      ...(remainingCommit?.commitStagedIndexAsIs ? { commitStagedIndexAsIs: true } : {}),
      action: remainingCommit === undefined ? "create_pr" : "commit_push_pr",
      onProgress,
    });
    if (AsyncResult.isSuccess(result)) {
      setProgress((current) =>
        current.kind === "created" || current.kind === "existing"
          ? current
          : reduceCreatePullRequestProgress(current, {
              actionId: "settled",
              cwd,
              action: "create_pr",
              kind: "action_finished",
              result: result.value,
            }),
      );
      onSettled(result.value);
      return;
    }
    const failure = squashAtomCommandFailure(result);
    setProgress((current) => failCreatePullRequestProgress(current, failureMessage(failure, noun)));
  }, [
    body,
    baseBranch,
    commitInput,
    createDefaults,
    createOptions,
    createOptionsSupported,
    providerKind,
    environmentId,
    progress.kind,
    refreshStatus,
    sourceBranch,
    refreshStatusQuery,
    cwd,
    onOpenChange,
    onProgress,
    onSettled,
    primaryDisabledReason,
    runStackedAction,
    settled,
    trimmedTitle,
    noun,
  ]);

  const handleOpenChange = useCallback(
    (nextOpen: boolean) => {
      if (!nextOpen && running) return;
      onOpenChange(nextOpen);
    },
    [onOpenChange, running],
  );
  const changeTitle = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setTitle(event.target.value);
  }, []);
  const changeBody = useCallback((event: ChangeEvent<HTMLTextAreaElement>) => {
    setBody(event.target.value);
  }, []);

  const outcomeUrl =
    progress.kind === "created" || progress.kind === "existing"
      ? safeExternalUrl(progress.url)
      : review?.existingPullRequest === null
        ? null
        : safeExternalUrl(review?.existingPullRequest?.url ?? null);
  const fieldsDisabled = busy || settled || review === null || blockedReason !== null;
  const draftHintId = useId();
  // An existing request's warning covers every option sent, including merge boxes filled from
  // the project's defaults; it only matters when the user chose something. Fields lock once
  // the request settles, so this still describes the attempt.
  const choseOptions =
    createOptionsSupported && createOptionsPayload(createOptions, providerKind) !== undefined;
  const branchFieldsDisabled =
    busy ||
    settled ||
    capabilityBlockedReason !== null ||
    status?.isRepo !== true ||
    provider === null;
  const statusText =
    presentation?.status ?? (review === null ? "Reading repository status…" : null);

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogPopup
        aria-busy={busy ? "true" : "false"}
        className="max-w-xl"
        data-testid="git-manager-create-pr-dialog"
      >
        <DialogHeader className="pb-4">
          <DialogTitle>Create {noun}</DialogTitle>
          <DialogDescription>
            {capabilityBlockedReason ?? `Review the ${noun} before it is created.`}
          </DialogDescription>
        </DialogHeader>
        <DialogPanel className="space-y-5" data-text-surface="popover">
          <section
            aria-label={`${capitalize(noun)} details`}
            className="rounded-xl border border-border/70 bg-muted/24 px-4"
            data-testid="create-pr-summary"
          >
            <dl className="divide-y divide-border/70 text-sm">
              <div className="grid grid-cols-[6rem_minmax(0,1fr)] items-start gap-4 py-2.5">
                <dt className="whitespace-nowrap text-muted-foreground">Repository</dt>
                <dd className="min-w-0 break-all text-right" data-testid="create-pr-repository">
                  {provider !== null
                    ? `${resolveChangeRequestPresentation(provider).providerName} · ${provider.baseUrl}`
                    : review === null
                      ? "…"
                      : status?.hasPrimaryRemote === false
                        ? "No origin remote"
                        : "Not identified yet"}
                </dd>
              </div>
            </dl>
          </section>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="git-manager-create-pr-head">Source branch</Label>
            <BranchPicker
              scope={scope}
              id="git-manager-create-pr-head"
              helpId="create-pr-source-help"
              placeholder="Select a source branch…"
              value={review?.head ?? null}
              onChange={setHeadBranch}
              search={headSearch ?? review?.head ?? ""}
              onSearchChange={setHeadSearch}
              disabled={branchFieldsDisabled || commitInput !== undefined}
            />
            <p id="create-pr-source-help" className="text-xs text-muted-foreground">
              {commitInput?.featureBranch
                ? `A new source branch will be created from ${review?.head ?? "the current branch"} for this commit.`
                : commitInput !== undefined
                  ? "Committing uses the current branch. To select another source, create a request without committing."
                  : "Defaults to the current branch, published first if origin does not have it. Other choices list branches on origin; your checkout stays unchanged."}
            </p>
          </div>
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="git-manager-create-pr-base">Target branch (required)</Label>
            <BranchPicker
              scope={scope}
              id="git-manager-create-pr-base"
              helpId={
                criticalTarget === null ? "create-pr-target-help" : "create-pr-target-warning"
              }
              placeholder="Select a target branch…"
              value={baseBranch}
              onChange={(value) => {
                setBaseBranch(value);
                selectedHead.current = review?.head ?? null;
              }}
              search={branchSearch}
              onSearchChange={setBranchSearch}
              excludedBranch={commitInput?.featureBranch ? null : (review?.head ?? null)}
              disabled={branchFieldsDisabled}
            />
            {criticalTarget === null ? (
              <p id="create-pr-target-help" className="text-xs text-muted-foreground">
                Choose the branch on origin that should receive these changes.
              </p>
            ) : (
              <Alert
                variant="error"
                id="create-pr-target-warning"
                className="mt-1"
                data-testid="create-pr-critical-target"
              >
                <TriangleAlertIcon aria-hidden />
                <AlertTitle>
                  Targets <span className="font-mono">{criticalTarget}</span>,{" "}
                  {targetIsDefault ? "the default branch" : "a mainline branch"}
                </AlertTitle>
                <AlertDescription className="text-xs">
                  Merging this {noun} changes the code everyone builds from. Check this is the
                  intended target.
                </AlertDescription>
              </Alert>
            )}
          </div>
          <div
            className="rounded-lg border border-border/70 px-3 py-2.5 text-xs"
            data-testid="create-pr-publish"
          >
            <p className="font-medium text-foreground">Branch publication</p>
            <p className="mt-1 text-muted-foreground">
              {review === null
                ? "Reading branch status…"
                : review.head !== status?.refName
                  ? `Any local commits on ${review.head ?? "the selected branch"} will be published to origin first.`
                  : review.publishRequired
                    ? `${review.head ?? "The branch"} is not on the remote yet and will be published first.`
                    : "The branch is already published."}
            </p>
          </div>
          {review?.existingPullRequest === null ||
          review?.existingPullRequest === undefined ? null : (
            <p
              className="rounded-lg border border-border/70 px-3 py-2.5 text-xs"
              data-testid="create-pr-existing"
            >
              {capitalize(noun)}{" "}
              {formatChangeRequestNumber(provider?.kind ?? null, review.existingPullRequest.number)}{" "}
              already exists for this branch:{" "}
              {outcomeUrl === null ? (
                review.existingPullRequest.title
              ) : (
                <a
                  className="underline-offset-2 hover:underline"
                  href={outcomeUrl}
                  rel="noreferrer"
                  target="_blank"
                >
                  {review.existingPullRequest.title}
                </a>
              )}
            </p>
          )}
          <section aria-label={`${capitalize(noun)} content`} className="space-y-4">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="git-manager-create-pr-title">Title</Label>
              <Input
                aria-invalid={trimmedTitle.length === 0 && review !== null ? "true" : undefined}
                disabled={fieldsDisabled}
                id="git-manager-create-pr-title"
                value={title}
                onChange={changeTitle}
              />
              {createOptionsSupported ? (
                <div className="pt-1">
                  <label className="flex items-center gap-2 text-sm">
                    <Checkbox
                      aria-describedby={draftHintId}
                      checked={createOptions.draft}
                      disabled={fieldsDisabled}
                      onCheckedChange={(draft) =>
                        setCreateOptions((current) => ({ ...current, draft }))
                      }
                    />
                    Mark as draft
                  </label>
                  <p id={draftHintId} className="ps-6 text-xs text-muted-foreground">
                    Drafts can't be merged until marked ready.
                  </p>
                </div>
              ) : null}
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="git-manager-create-pr-body">Description</Label>
              <Textarea
                disabled={fieldsDisabled}
                id="git-manager-create-pr-body"
                rows={4}
                value={body}
                onChange={changeBody}
              />
            </div>
            {createOptionsSupported ? (
              <CreatePullRequestOptions
                scope={scope}
                providerKind={providerKind}
                defaults={createDefaults}
                defaultsState={createDefaultsState}
                onRetryDefaults={createDefaultsQuery.refresh}
                value={createOptions}
                onChange={setCreateOptions}
                disabled={fieldsDisabled}
              />
            ) : null}
          </section>
          {statusText === null && review?.blockedReason == null ? null : (
            <p
              aria-live="polite"
              className={
                presentation?.tone === "error"
                  ? "text-xs text-destructive"
                  : "text-xs text-muted-foreground"
              }
              data-testid="create-pr-status"
              ref={statusRef}
              role="status"
            >
              {statusText ?? review?.blockedReason}
              {outcomeUrl !== null && settled ? (
                <>
                  {" "}
                  <a
                    className="underline-offset-2 hover:underline"
                    href={outcomeUrl}
                    rel="noreferrer"
                    target="_blank"
                  >
                    Open {noun}
                  </a>
                </>
              ) : null}
              {(progress.kind === "created" || (progress.kind === "existing" && choseOptions)) &&
              progress.warning !== undefined ? (
                <span className="mt-1 block font-medium text-foreground">{progress.warning}</span>
              ) : null}
            </p>
          )}
        </DialogPanel>
        <DialogFooter>
          <Button
            disabled={busy}
            title={busy ? waitReason : undefined}
            variant="outline"
            onClick={() => handleOpenChange(false)}
          >
            {settled ? "Close" : "Cancel"}
          </Button>
          <PermissionButton
            permission={{ allowed: primaryDisabledReason === null, reason: primaryDisabledReason }}
            onClick={() => {
              void submit();
            }}
          >
            <GitPullRequestIcon aria-hidden="true" />
            {commitInput !== undefined && progress.kind === "review"
              ? `Commit, publish and create ${noun}`
              : (presentation?.primaryLabel ?? `Create ${noun}`)}
          </PermissionButton>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}

/** Shared branch lookup keeps both selectors searchable and paginated. */
function BranchPicker({
  scope,
  id,
  helpId,
  placeholder,
  value,
  onChange,
  search,
  onSearchChange,
  excludedBranch = null,
  disabled,
}: {
  readonly scope: GitManagerCreatePullRequestDialogProps["scope"];
  readonly id: string;
  readonly helpId: string;
  readonly placeholder: string;
  readonly value: string | null;
  readonly onChange: (value: string | null) => void;
  readonly search: string;
  readonly onSearchChange: (value: string) => void;
  readonly excludedBranch?: string | null;
  readonly disabled: boolean;
}) {
  const { environmentId, cwd } = scope;
  const filterSearch = search === value ? "" : search.trim();
  const [branchQuery] = useDebouncedValue(filterSearch, { wait: 200 });
  const [branchPage, setBranchPage] = useState({ query: "", cursor: 0 });
  const branchCursor = branchPage.query === branchQuery ? branchPage.cursor : 0;
  const refsAtom = useMemo(
    () =>
      vcsEnvironment.listRefs({
        environmentId,
        input: {
          cwd,
          limit: 100,
          cursor: branchCursor,
          includeMatchingRemoteRefs: true,
          refKind: "remote",
          ...(branchQuery ? { query: branchQuery } : {}),
        },
      }),
    [branchCursor, branchQuery, cwd, environmentId],
  );
  const refsQuery = useEnvironmentQuery(refsAtom);
  const searchingBranches = refsQuery.isPending || branchQuery !== filterSearch;
  const branches = useMemo(
    () =>
      [
        ...new Set(
          (refsQuery.data?.refs ?? []).flatMap((ref) => {
            if (!ref.isRemote || ref.remoteName !== "origin") return [];
            const name = ref.name.replace(/^origin\//, "");
            return name === "HEAD" || name === excludedBranch ? [] : [name];
          }),
        ),
      ]
        .filter((name) =>
          name.toLowerCase().includes(
            filterSearch
              .trim()
              .replace(/^origin\//, "")
              .toLowerCase(),
          ),
        )
        .sort(),
    [filterSearch, excludedBranch, refsQuery.data],
  );
  return (
    <Combobox
      items={branches}
      filter={null}
      value={value}
      inputValue={search}
      onValueChange={(value) => {
        onChange(value);
      }}
      onInputValueChange={(value, details) => {
        onSearchChange(value);
        if (details.reason === "input-change") onChange(null);
      }}
      disabled={disabled}
    >
      <ComboboxInput
        id={id}
        placeholder={placeholder}
        aria-required="true"
        aria-describedby={helpId}
        maxLength={256}
        showClear
      />
      <ComboboxPopup>
        <ComboboxList data-text-surface="popover">
          {(branch: string) => (
            <ComboboxItem key={branch} value={branch}>
              {branch}
            </ComboboxItem>
          )}
        </ComboboxList>
        {searchingBranches ? (
          <p role="status" className="p-2 text-sm">
            Loading branches…
          </p>
        ) : null}
        {refsQuery.error ? (
          <div className="p-2 text-sm">
            <p role="alert">Could not load branches.</p>
            <Button size="sm" onClick={refsQuery.refresh}>
              Retry
            </Button>
          </div>
        ) : null}
        {!searchingBranches && !refsQuery.error && branches.length === 0 ? (
          <p className="p-2 text-sm text-muted-foreground">
            No matching branches on origin. Fetch the repository to update the list.
          </p>
        ) : null}
        {branchCursor > 0 || refsQuery.data?.nextCursor != null ? (
          <div className="flex justify-between gap-2 p-2">
            <Button
              size="sm"
              variant="ghost"
              disabled={branchCursor === 0 || searchingBranches}
              onClick={() =>
                setBranchPage({
                  query: branchQuery,
                  cursor: Math.max(0, branchCursor - 100),
                })
              }
            >
              Previous branches
            </Button>
            <Button
              size="sm"
              variant="ghost"
              disabled={refsQuery.data?.nextCursor == null || searchingBranches}
              onClick={() => {
                if (refsQuery.data?.nextCursor != null)
                  setBranchPage({ query: branchQuery, cursor: refsQuery.data.nextCursor });
              }}
            >
              Next branches
            </Button>
          </div>
        ) : null}
      </ComboboxPopup>
    </Combobox>
  );
}
