import type { PullRequestsDetail, ScopedProjectRef } from "@coddy/contracts";
import type { PullRequestsScope } from "../usePullRequestsAction";
import { useState } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { Button } from "../../ui/button";
import {
  Combobox,
  ComboboxInput,
  ComboboxPopup,
  ComboboxList,
  ComboboxItem,
} from "../../ui/combobox";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { pullRequestsActionError, usePullRequestsActions } from "../usePullRequestsAction";
import { usePullRequestsVocabulary } from "./usePullRequestsVocabulary";
import { PullRequestsConfirmAction } from "./PullRequestsConfirmAction";

function BranchChoices({
  scope,
  onChoose,
  onClose,
}: {
  scope: PullRequestsScope;
  onChoose: (value: string) => void;
  onClose: () => void;
}) {
  const query = usePullRequestsVocabulary(scope, "branches");
  return (
    <Combobox
      items={query.entries.map((entry) => entry.label)}
      filter={null}
      defaultOpen
      value={null}
      inputValue={query.search}
      onInputValueChange={query.onSearch}
      onValueChange={(value) => {
        if (value) onChoose(value);
      }}
      onOpenChange={(open) => {
        if (!open) onClose();
      }}
    >
      <ComboboxInput autoFocus aria-label="Base branch" className="w-56" />
      <ComboboxPopup data-text-surface="popover">
        <ComboboxList>
          {(branch: string) => (
            <ComboboxItem key={branch} value={branch}>
              {branch}
            </ComboboxItem>
          )}
        </ComboboxList>
        {query.searching ? (
          <p role="status" className="p-2 text-sm">
            Loading branches…
          </p>
        ) : null}
        {query.error ? (
          <div className="p-2 text-sm">
            <p role="alert">{query.error}</p>
            <Button size="sm" onClick={query.refresh}>
              Retry
            </Button>
          </div>
        ) : null}
        {!query.searching && !query.error && !query.entries.length ? (
          <p className="p-2 text-sm text-muted-foreground">No matching branches</p>
        ) : null}
      </ComboboxPopup>
    </Combobox>
  );
}
export function PullRequestsBaseBranchPicker({
  detail,
  live = detail,
  scope,
  projectRef,
}: {
  detail: PullRequestsDetail;
  /**
   * Changing the base branch only ever acts on a live detail, never a
   * snapshot. Omitting this falls back to `detail`, so callers that have not
   * adopted the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
  scope: PullRequestsScope;
  projectRef: ScopedProjectRef;
}) {
  const { run, pending, requestKind } = usePullRequestsActions();
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const comments = usePullRequestsStore(
    (s) => s.selectDraft(projectRef, detail.number).pendingReview.length,
  );
  const permission = constrainPermission(
    detail.permissions.editPullRequest,
    live === null ? "Loading…" : pending ? "Wait for the current action to finish" : null,
  );
  async function change(baseBranch: string) {
    await run({ action: "editPullRequest", title: null, body: null, baseBranch });
    usePullRequestsStore.getState().setPendingReview(projectRef, detail.number, []);
  }
  return (
    <>
      {open ? (
        <BranchChoices
          scope={scope}
          onClose={() => setOpen(false)}
          onChoose={(value) => {
            setOpen(false);
            setError(null);
            if (value === detail.baseBranch || !permission.allowed) return;
            if (comments) setTarget(value);
            else
              void change(value).catch((cause) =>
                setError(pullRequestsActionError(cause, requestKind)),
              );
          }}
        />
      ) : (
        <PermissionButton
          mutation
          permission={permission}
          size="sm"
          variant="secondary"
          aria-label="Change base branch"
          onClick={() => setOpen(true)}
        >
          <code className="text-xs">{detail.baseBranch}</code>
        </PermissionButton>
      )}
      {error ? (
        <span role="alert" className="text-sm text-destructive">
          {error}
        </span>
      ) : null}
      {target ? (
        <PullRequestsConfirmAction
          title="Change base branch"
          confirmLabel="Change base"
          permission={permission}
          onClose={() => setTarget(null)}
          onConfirm={() => change(target)}
        >
          <p>
            Change the base from {detail.baseBranch} to {target}.
          </p>
          <p>
            Changing the base clears {comments} pending review comment{comments === 1 ? "" : "s"}.
          </p>
        </PullRequestsConfirmAction>
      ) : null}
    </>
  );
}
