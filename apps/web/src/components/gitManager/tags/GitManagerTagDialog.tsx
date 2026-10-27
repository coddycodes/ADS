import { RegistryContext } from "@effect/atom-react";
import type {
  EnvironmentId,
  GitManagerOperationEvent,
  GitManagerOperationRequest,
  ScopedProjectRef,
} from "@coddy/contracts";
import * as Cause from "effect/Cause";
import { TagIcon } from "lucide-react";
import {
  memo,
  type ChangeEvent,
  type KeyboardEvent,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
} from "react";

import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPopup,
  DialogTitle,
} from "~/components/ui/dialog";
import { Checkbox } from "~/components/ui/checkbox";
import { Input } from "~/components/ui/input";
import { runGitManagerOperation, type GitManagerOperationHandle } from "~/state/gitManager";

import { GitManagerOperationBanner } from "../toolbar/GitManagerOperationBanner";
import { resolveTagDeleteDialogCopy, validateTagName } from "./GitManagerTagDialog.logic";

const noop = () => undefined;

export interface GitManagerTagDialogProps {
  readonly open: boolean;
  readonly action: "create" | "delete" | "push";
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly projectRef: ScopedProjectRef;
  readonly existingTags: ReadonlyArray<string>;
  readonly targetSha: string | null;
  readonly tag: string | null;
  readonly remote: string | null;
  readonly disabledReason?: string | null;
  readonly onOpenChange: (open: boolean) => void;
  readonly onFinished?: () => void;
}

export const GitManagerTagDialog = memo(function GitManagerTagDialog({
  open,
  action,
  scope,
  projectRef,
  existingTags,
  targetSha,
  tag,
  remote,
  disabledReason: capabilityDisabledReason = null,
  onOpenChange,
  onFinished = noop,
}: GitManagerTagDialogProps) {
  const registry = useContext(RegistryContext);
  const [name, setName] = useState("");
  const [pendingTagName, setPendingTagName] = useState<string | null>(null);
  const [operationEvent, setOperationEvent] = useState<GitManagerOperationEvent | null>(null);
  const [failureMessage, setFailureMessage] = useState<string | null>(null);
  const activeOperationRef = useRef<GitManagerOperationHandle | null>(null);
  useEffect(
    () => () => {
      activeOperationRef.current?.cancel();
    },
    [],
  );

  const [pushAfterCreate, setPushAfterCreate] = useState(false);
  const selectedTag = action === "create" ? name : (tag ?? "");
  const validation = validateTagName(selectedTag, action === "create" ? existingTags : []);
  const missingOperand =
    action === "create" && targetSha === null
      ? "Choose a commit for the new tag."
      : action === "push" && remote === null
        ? "Choose a remote for the tag."
        : action !== "create" && tag === null
          ? "Choose a tag."
          : null;
  const disabledReason =
    capabilityDisabledReason ??
    (pendingTagName === null ? null : `The ${pendingTagName} tag operation is running.`) ??
    missingOperand ??
    validation.reason;
  const deleteCopy = action === "delete" ? resolveTagDeleteDialogCopy(selectedTag) : null;
  const title =
    action === "create"
      ? "Create Tag"
      : action === "delete"
        ? (deleteCopy?.title ?? "Delete Tag")
        : `Push tag ${selectedTag}?`;
  const description =
    action === "create"
      ? "Create an annotated tag at the selected commit."
      : action === "delete"
        ? (deleteCopy?.description ?? "Delete the selected local tag.")
        : `Push refs/tags/${selectedTag} to ${remote ?? "the selected remote"}.`;
  const confirmLabel =
    action === "create"
      ? "Create Tag"
      : action === "delete"
        ? (deleteCopy?.confirmLabel ?? "Delete Tag")
        : "Push Tag";

  const changeName = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setName(event.currentTarget.value);
    setFailureMessage(null);
  }, []);
  const cancelOperation = useCallback(() => {
    activeOperationRef.current?.cancel();
    activeOperationRef.current = null;
    setPendingTagName(null);
  }, []);
  const confirm = useCallback(() => {
    if (
      disabledReason !== null ||
      activeOperationRef.current !== null ||
      selectedTag.length === 0
    ) {
      return;
    }
    let input: GitManagerOperationRequest;
    if (action === "create") {
      if (targetSha === null) return;
      input = {
        _tag: "tag-create",
        cwd: scope.cwd,
        projectId: projectRef.projectId,
        name: selectedTag,
        sha: targetSha,
        pushRemote: pushAfterCreate && remote !== null ? remote : null,
      };
    } else if (action === "delete") {
      input = {
        _tag: "tag-delete",
        cwd: scope.cwd,
        projectId: projectRef.projectId,
        name: selectedTag,
      };
    } else {
      if (remote === null) return;
      input = {
        _tag: "tag-push",
        cwd: scope.cwd,
        projectId: projectRef.projectId,
        name: selectedTag,
        remote,
      };
    }
    setFailureMessage(null);
    setPendingTagName(selectedTag);
    setOperationEvent({ _tag: "started", operation: input._tag });
    const handle = runGitManagerOperation(
      registry,
      { environmentId: scope.environmentId, input },
      (event) => {
        setOperationEvent(event);
        if (event._tag === "failed") {
          setPendingTagName(null);
          setFailureMessage(event.blocked?.message ?? event.message);
        } else if (event._tag === "finished") {
          setPendingTagName(null);
          setName("");
          onFinished();
          onOpenChange(false);
        }
      },
    );
    activeOperationRef.current = handle;
    void handle.result.then((result) => {
      if (activeOperationRef.current === handle) activeOperationRef.current = null;
      if (result._tag !== "Failure" || Cause.hasInterruptsOnly(result.cause)) return;
      const error = Cause.squash(result.cause);
      const message = error instanceof Error ? error.message : "The tag operation failed.";
      setPendingTagName(null);
      setFailureMessage(message);
      setOperationEvent({
        _tag: "failed",
        operation: input._tag,
        code: "transport-error",
        message,
        blocked: null,
      });
    });
  }, [
    action,
    disabledReason,
    onFinished,
    onOpenChange,
    projectRef.projectId,
    pushAfterCreate,
    registry,
    remote,
    scope.cwd,
    scope.environmentId,
    selectedTag,
    targetSha,
  ]);
  const submitName = useCallback(
    (event: KeyboardEvent<HTMLInputElement>) => {
      if (event.key !== "Enter") return;
      event.preventDefault();
      confirm();
    },
    [confirm],
  );

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogPopup>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        <div className="space-y-3 px-6 pb-4">
          {action === "create" ? (
            <>
              <label className="block space-y-1">
                <span className="text-xs font-medium">Tag name</span>
                <Input
                  aria-describedby="git-manager-tag-name-reason"
                  aria-label="Tag name"
                  autoComplete="off"
                  name="tag-name"
                  spellCheck={false}
                  value={name}
                  onChange={changeName}
                  onKeyDown={submitName}
                />
              </label>
              {remote === null ? null : (
                <label className="flex items-center gap-2 text-xs">
                  <Checkbox
                    aria-label={`Push to ${remote} after creating`}
                    checked={pushAfterCreate}
                    onCheckedChange={(checked) => setPushAfterCreate(checked === true)}
                  />
                  <span>
                    Push to <span className="font-mono">{remote}</span> after creating
                  </span>
                </label>
              )}
            </>
          ) : (
            <p className="flex items-center gap-2 rounded-md border border-border px-3 py-2 font-mono text-xs">
              <TagIcon aria-hidden="true" className="size-3.5" />
              {selectedTag}
            </p>
          )}
          {disabledReason === null ? null : (
            <p className="text-xs text-muted-foreground" id="git-manager-tag-name-reason">
              {disabledReason}
            </p>
          )}
          {failureMessage === null ? null : (
            <p aria-live="polite" className="text-xs text-destructive">
              {failureMessage}
            </p>
          )}
          <GitManagerOperationBanner operation={operationEvent} onCancel={cancelOperation} />
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            aria-describedby={disabledReason === null ? undefined : "git-manager-tag-name-reason"}
            disabled={disabledReason !== null}
            title={disabledReason ?? undefined}
            variant={action === "delete" ? "destructive" : "default"}
            onClick={confirm}
          >
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
});
