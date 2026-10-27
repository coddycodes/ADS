import { getChangeRequestTerminologyForKind } from "@coddy/shared/sourceControl";
import type {
  PullRequestsContext,
  PullRequestsDetail,
  PullRequestsMergeMethod,
  PullRequestsPermission,
  ScopedProjectRef,
} from "@coddy/contracts";
import { ChevronDownIcon } from "lucide-react";
import { useContext, useId, useState } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import { Menu, MenuTrigger, MenuPopup, MenuItem } from "../../ui/menu";
import { Select, SelectTrigger, SelectValue, SelectPopup, SelectItem } from "../../ui/select";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { MutationsDisabledContext } from "../../ui/mutationAvailability";
import { usePullRequestsActions, type PullRequestsAction } from "../usePullRequestsAction";
import { PullRequestsConfirmAction } from "./PullRequestsConfirmAction";

const METHODS = { merge: "Merge commit", squash: "Squash and merge", rebase: "Rebase and merge" };
type Host = Extract<PullRequestsContext, { status: "available" }>;
type MergeAction = Extract<PullRequestsAction, { action: "merge" }>;
function MergeOption({
  label,
  checked,
  onChange,
  permission,
}: {
  label: string;
  checked: boolean;
  onChange: (value: boolean) => void;
  permission: PullRequestsPermission;
}) {
  const id = useId();
  return (
    <div>
      <label className="flex min-h-8 items-center gap-2" title={permission.reason ?? undefined}>
        <input
          type="checkbox"
          aria-label={label}
          checked={checked}
          disabled={!permission.allowed}
          aria-describedby={!permission.allowed ? id : undefined}
          onChange={(event) => onChange(event.target.checked)}
        />
        {label}
      </label>
      {!permission.allowed ? (
        <p id={id} className="text-xs text-muted-foreground">
          {permission.reason}
        </p>
      ) : null}
    </div>
  );
}

export function PullRequestsUpdateBranch({
  detail,
  live = detail,
  context,
}: {
  detail: PullRequestsDetail;
  /**
   * Updating the branch only ever acts on a live detail, never a snapshot.
   * Omitting this falls back to `detail`, so callers that have not adopted
   * the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
  context: Host;
}) {
  const { run, pending } = usePullRequestsActions();
  const [skipCi, setSkipCi] = useState(false);
  const disabledReason = useContext(MutationsDisabledContext);
  const permission = constrainPermission(
    detail.permissions.updateBranch,
    live === null
      ? "Loading…"
      : (disabledReason ?? (pending ? "Wait for the current action to finish" : null)),
  );
  const methods = detail.permissions.updateBranch.methods;
  const method = methods[0];
  const rebaseOnly = context.provider === "gitlab" || method === "rebase";
  const label = rebaseOnly ? "Rebase" : "Update branch";
  function update(method: "merge" | "rebase") {
    void run({
      action: "updateBranch",
      method,
      skipCi: context.provider === "gitlab" && method === "rebase" && skipCi,
    }).catch(() => undefined);
  }
  return (
    <div className="space-y-2 text-sm">
      {context.provider === "gitlab" ? (
        <MergeOption
          label="Skip CI"
          checked={skipCi}
          onChange={setSkipCi}
          permission={permission}
        />
      ) : null}
      <div className="flex items-center gap-1">
        <PermissionButton
          mutation
          permission={constrainPermission(
            permission,
            method ? null : "No branch update method is available",
          )}
          variant="outline"
          size="sm"
          onClick={() => {
            if (method) update(method);
          }}
        >
          {label}
        </PermissionButton>
        {methods.length > 1 ? (
          <Menu>
            <MenuTrigger
              nativeButton
              render={
                <Button
                  variant="outline"
                  size="icon-sm"
                  aria-label="Update branch options"
                  disabled={!permission.allowed}
                />
              }
            >
              <ChevronDownIcon aria-hidden="true" />
            </MenuTrigger>
            <MenuPopup>
              {methods.map((value) => (
                <MenuItem
                  key={value}
                  nativeButton
                  render={
                    <PermissionButton
                      mutation
                      permission={permission}
                      variant="ghost"
                      onClick={() => update(value)}
                    />
                  }
                >
                  {value === "merge" ? "Update with merge" : "Rebase"}
                </MenuItem>
              ))}
            </MenuPopup>
          </Menu>
        ) : null}
      </div>
    </div>
  );
}

export function PullRequestsMergeControls({
  detail,
  live = detail,
  context,
  projectRef,
}: {
  detail: PullRequestsDetail;
  /**
   * Merging only ever acts on a live detail, never a snapshot. Omitting this
   * falls back to `detail`, so callers that have not adopted the split keep
   * acting as before.
   */
  live?: PullRequestsDetail | null;
  context: Host;
  projectRef: ScopedProjectRef;
}) {
  const { run, pending, error } = usePullRequestsActions();
  const disabledReason = useContext(MutationsDisabledContext);
  const edited = usePullRequestsStore(
    (s) => s.selectDraft(projectRef, detail.number).mergeDraftEdited,
  );
  const savedSubject = usePullRequestsStore(
    (s) => s.selectDraft(projectRef, detail.number).mergeSubject,
  );
  const savedBody = usePullRequestsStore((s) => s.selectDraft(projectRef, detail.number).mergeBody);
  const [chosenMethod, setMethod] = useState<PullRequestsMergeMethod | null>(null);
  const [deleteBranch, setDeleteBranch] = useState(detail.permissions.merge.deleteBranchDefault);
  const [auto, setAuto] = useState(false);
  const [confirmation, setConfirmation] = useState<{
    action: MergeAction;
    baseBranch: string;
    headBranch: string;
    draftSubject: string;
    draftBody: string;
  } | null>(null);
  const merge = detail.permissions.merge;
  const methods = merge.methods;
  const defaultMethod =
    merge.defaultMethod && methods.includes(merge.defaultMethod)
      ? merge.defaultMethod
      : methods.length === 1
        ? methods[0]!
        : null;
  const method =
    context.capabilities.mergeMethodsSource === "project_setting"
      ? defaultMethod
      : chosenMethod && methods.includes(chosenMethod)
        ? chosenMethod
        : defaultMethod;
  const activeAuto = detail.readiness.autoMerge?.enabled ?? false;
  const effectiveAuto = auto && !activeAuto;
  const canConfigure =
    merge.allowed ||
    detail.permissions.mergeBypass.allowed ||
    detail.permissions.enableAutoMerge.allowed;
  const busyReason =
    live === null
      ? "Loading…"
      : (disabledReason ?? (pending ? "Wait for the current action to finish" : null));
  const configuration = constrainPermission(
    { allowed: canConfigure, reason: canConfigure ? null : merge.reason },
    busyReason,
  );
  const methodReason = !method
    ? "Choose a merge method"
    : !detail.headSha
      ? `Reload this ${getChangeRequestTerminologyForKind(context.provider).singular} before merging`
      : null;
  function actionPermission(action: { auto: boolean; bypass: boolean }, reason: string | null) {
    let permission = action.bypass
      ? detail.permissions.mergeBypass
      : action.auto
        ? detail.permissions.enableAutoMerge
        : merge;
    if (permission.allowed && action.bypass && action.auto)
      permission =
        context.provider === "github"
          ? { allowed: false, reason: "Auto-merge and bypass cannot be combined on GitHub." }
          : detail.permissions.enableAutoMerge;
    return constrainPermission(permission, busyReason ?? reason);
  }
  const primaryPermission = actionPermission({ auto: effectiveAuto, bypass: false }, methodReason);
  const bypassPermission = actionPermission({ auto: effectiveAuto, bypass: true }, methodReason);
  const bypassLabel =
    context.provider === "gitlab"
      ? "Merge despite requested changes"
      : "Merge without waiting for requirements";
  const hasMessage = !(context.provider === "github" && method === "rebase");
  const subject = edited
    ? savedSubject
    : context.provider === "github" && hasMessage
      ? `${detail.title} (#${detail.number})`
      : "";
  const body = edited ? savedBody : "";
  function draft(subject: string, body: string) {
    usePullRequestsStore.getState().setMergeDraft(projectRef, detail.number, subject, body);
  }
  function confirm(bypass: boolean) {
    if (!method || !(bypass ? bypassPermission : primaryPermission).allowed) return;
    setConfirmation({
      action: {
        action: "merge",
        method,
        deleteBranch,
        auto: effectiveAuto,
        bypass,
        headSha: detail.headSha,
        subject: hasMessage && subject.trim() ? subject : null,
        body: hasMessage && body.trim() ? body : null,
      },
      baseBranch: detail.baseBranch,
      headBranch: detail.headBranch,
      draftSubject: savedSubject,
      draftBody: savedBody,
    });
  }
  return (
    <div className="space-y-3 text-sm">
      {canConfigure && methods.length ? (
        <>
          {context.capabilities.mergeMethodsSource === "project_setting" ? (
            <p>Project merge method: {method ? METHODS[method] : "Unavailable"}</p>
          ) : methods.length > 1 ? (
            <div className="max-w-xs">
              <Select
                value={method}
                disabled={!configuration.allowed}
                onValueChange={(value) => {
                  if (value && methods.includes(value)) setMethod(value);
                }}
              >
                <SelectTrigger aria-label="Merge method">
                  <SelectValue>{method ? METHODS[method] : "Choose merge method"}</SelectValue>
                </SelectTrigger>
                <SelectPopup>
                  {methods.map((value) => (
                    <SelectItem key={value} value={value}>
                      {METHODS[value]}
                    </SelectItem>
                  ))}
                </SelectPopup>
              </Select>
            </div>
          ) : (
            <p>Merge method: {METHODS[methods[0]!]}</p>
          )}
          {hasMessage ? (
            <div className="space-y-2">
              <label className="block space-y-1">
                <span>Merge subject</span>
                <Input
                  aria-label="Merge subject"
                  value={subject}
                  readOnly={!configuration.allowed}
                  placeholder={
                    context.provider === "gitlab"
                      ? `Merge branch '${detail.headBranch}' into '${detail.baseBranch}'`
                      : undefined
                  }
                  onChange={(event) => draft(event.target.value, body)}
                />
              </label>
              <label className="block space-y-1">
                <span>Merge body</span>
                <textarea
                  aria-label="Merge body"
                  className="min-h-24 w-full resize-y rounded-md border border-border bg-background p-3 text-sm"
                  value={body}
                  readOnly={!configuration.allowed}
                  onChange={(event) => draft(subject, event.target.value)}
                />
              </label>
            </div>
          ) : null}
          <MergeOption
            label="Delete branch after merge"
            checked={deleteBranch}
            onChange={setDeleteBranch}
            permission={configuration}
          />
          {!activeAuto ? (
            <MergeOption
              label={context.capabilities.autoMergeLabel}
              checked={effectiveAuto}
              onChange={setAuto}
              permission={constrainPermission(detail.permissions.enableAutoMerge, busyReason)}
            />
          ) : null}
        </>
      ) : (
        <p className="text-muted-foreground">{merge.reason}</p>
      )}
      {activeAuto ? (
        <div className="flex flex-wrap items-center gap-2">
          <span>Auto-merge enabled</span>
          <PermissionButton
            mutation
            permission={constrainPermission(detail.permissions.disableAutoMerge, busyReason)}
            variant="outline"
            size="sm"
            onClick={() => {
              void run({ action: "disableAutoMerge" }).catch(() => undefined);
            }}
          >
            Disable auto-merge
          </PermissionButton>
        </div>
      ) : null}
      <div className="flex flex-wrap gap-2">
        <PermissionButton
          mutation
          permission={primaryPermission}
          size="sm"
          onClick={() => confirm(false)}
        >
          Merge
        </PermissionButton>
        {detail.permissions.mergeBypass.allowed ? (
          <PermissionButton
            mutation
            permission={bypassPermission}
            variant="outline"
            size="sm"
            onClick={() => confirm(true)}
          >
            {bypassLabel}
          </PermissionButton>
        ) : null}
      </div>
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      {confirmation ? (
        <PullRequestsConfirmAction
          title={
            confirmation.action.bypass
              ? bypassLabel
              : confirmation.action.auto
                ? "Enable auto-merge"
                : `Merge ${getChangeRequestTerminologyForKind(context.provider).singular}`
          }
          confirmLabel={
            confirmation.action.bypass
              ? bypassLabel
              : confirmation.action.auto
                ? "Enable auto-merge"
                : "Merge"
          }
          permission={actionPermission(
            confirmation.action,
            methods.includes(confirmation.action.method)
              ? null
              : "This merge method is no longer available",
          )}
          destructive={confirmation.action.bypass}
          onClose={() => setConfirmation(null)}
          onConfirm={async () => {
            await run(confirmation.action);
            const store = usePullRequestsStore.getState();
            const latest = store.selectDraft(projectRef, detail.number);
            if (
              latest.mergeSubject === confirmation.draftSubject &&
              latest.mergeBody === confirmation.draftBody
            )
              store.setMergeDraft(projectRef, detail.number, null, null);
          }}
        >
          <p>
            {confirmation.action.bypass
              ? bypassLabel
              : confirmation.action.auto
                ? "Merge automatically when requirements pass"
                : "Merge now"}{" "}
            into <strong>{confirmation.baseBranch}</strong>.
          </p>
          <ul className="mt-2 space-y-1">
            <li>Method: {METHODS[confirmation.action.method]}</li>
            <li>
              Delete branch:{" "}
              {confirmation.action.deleteBranch ? `Yes (${confirmation.headBranch})` : "No"}
            </li>
            <li>Auto-merge: {confirmation.action.auto ? "Yes" : "No"}</li>
          </ul>
          {confirmation.action.bypass ? (
            <p className="mt-2">
              {context.provider === "gitlab"
                ? "Requested changes will be overridden."
                : "Branch requirements will be bypassed."}
            </p>
          ) : null}
        </PullRequestsConfirmAction>
      ) : null}
    </div>
  );
}
