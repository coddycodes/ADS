import type { PullRequestsPermission, ScopedProjectRef } from "@coddy/contracts";
import { useId, useLayoutEffect, useRef, useState } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { Tabs, TabsList, TabsPanel, TabsTab } from "../../ui/tabs";
import { PullRequestsMarkdown } from "../shared/PullRequestsMarkdown";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { usePullRequestsActions } from "../usePullRequestsAction";
export interface PullRequestsCommentBoxProps {
  permission: PullRequestsPermission;
  draft: string;
  onDraftChange: (draft: string) => void;
  onSubmit: (body: string) => Promise<unknown>;
  label?: string;
  submitLabel?: string;
  busy?: boolean;
}
export function PullRequestsCommentBox({
  permission,
  draft,
  onDraftChange,
  onSubmit,
  label = "Comment",
  submitLabel = "Comment",
  busy = false,
}: PullRequestsCommentBoxProps) {
  const id = useId();
  const textarea = useRef<HTMLTextAreaElement>(null);
  const sending = useRef(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState("write");
  useLayoutEffect(() => {
    const input = textarea.current;
    if (input && tab === "write") {
      input.style.height = "auto";
      input.style.height = `${draft ? Math.max(96, input.scrollHeight) : 96}px`;
    }
  }, [draft, tab]);
  const submitPermission = constrainPermission(
    permission,
    busy || pending
      ? "Wait for the current action to finish"
      : !draft.trim()
        ? "Write a comment first"
        : null,
  );
  async function submit() {
    if (!submitPermission.allowed || sending.current) return;
    sending.current = true;
    setPending(true);
    setError(null);
    try {
      await onSubmit(draft);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Could not post the comment. Try again.");
    } finally {
      sending.current = false;
      setPending(false);
    }
  }
  return (
    <section className="space-y-2" aria-label={label}>
      <label htmlFor={id} className="block text-sm font-medium">
        {label}
      </label>
      <Tabs value={tab} onValueChange={(value) => setTab(String(value))} className="gap-2">
        <TabsList aria-label={`${label} editor`}>
          <TabsTab value="write">Write</TabsTab>
          <TabsTab value="preview">Preview</TabsTab>
        </TabsList>
        <TabsPanel value="write">
          <textarea
            id={id}
            ref={textarea}
            value={draft}
            disabled={!permission.allowed}
            readOnly={pending || busy}
            title={permission.reason ?? undefined}
            aria-describedby={!permission.allowed ? `${id}-reason` : undefined}
            className="min-h-24 w-full resize-y rounded-lg border border-border bg-background p-3 text-sm"
            onChange={(event) => onDraftChange(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (
                event.key === "Enter" &&
                (event.ctrlKey || event.metaKey) &&
                !event.nativeEvent.isComposing
              ) {
                event.preventDefault();
                void submit();
              }
            }}
          />
        </TabsPanel>
        <TabsPanel value="preview" className="min-h-24 rounded-lg border border-border p-3">
          {draft.trim() ? (
            <PullRequestsMarkdown text={draft} />
          ) : (
            <p className="text-sm text-muted-foreground">Nothing to preview</p>
          )}
        </TabsPanel>
      </Tabs>
      {!permission.allowed ? (
        <p id={`${id}-reason`} className="text-xs text-muted-foreground">
          {permission.reason}
        </p>
      ) : null}
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <div className="flex justify-end">
        <PermissionButton mutation permission={submitPermission} onClick={() => void submit()}>
          {pending ? "Posting…" : submitLabel}
        </PermissionButton>
      </div>
    </section>
  );
}
export function PullRequestsConversationComment({
  permission,
  projectRef,
  number,
  live = true,
}: {
  permission: PullRequestsPermission;
  projectRef: ScopedProjectRef;
  number: number;
  /**
   * Commenting only ever acts on a live detail, never a snapshot. Defaults
   * to `true` so callers that have not adopted the split keep acting as
   * before.
   */
  live?: boolean;
}) {
  const draft = usePullRequestsStore((s) => s.selectDraft(projectRef, number).comment);
  const { run, pending } = usePullRequestsActions();
  return (
    <PullRequestsCommentBox
      permission={constrainPermission(permission, !live ? "Loading…" : null)}
      draft={draft}
      busy={pending}
      onDraftChange={(value) =>
        usePullRequestsStore.getState().setCommentDraft(projectRef, number, value)
      }
      onSubmit={async (body) => {
        await run({ action: "comment", body });
        const store = usePullRequestsStore.getState();
        if (store.selectDraft(projectRef, number).comment === body)
          store.setCommentDraft(projectRef, number, "");
      }}
    />
  );
}
