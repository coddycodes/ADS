import type { PullRequestsDetail, ScopedProjectRef } from "@coddy/contracts";
import { PencilIcon } from "lucide-react";
import { useRef, useState, type ReactNode } from "react";
import { usePullRequestsStore } from "../../../pullRequestsStore";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import { Tabs, TabsList, TabsTab, TabsPanel } from "../../ui/tabs";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { PullRequestsMarkdown } from "../shared/PullRequestsMarkdown";
import { pullRequestsActionError, usePullRequestsActions } from "../usePullRequestsAction";

export interface PullRequestsEditorProps {
  detail: PullRequestsDetail;
  projectRef: ScopedProjectRef;
  baseUrl?: string;
  /**
   * Editing the title or body only ever acts on a live detail, never a
   * snapshot. Omitting this falls back to `detail`, so callers that have
   * not adopted the split keep acting as before.
   */
  live?: PullRequestsDetail | null;
}
export function PullRequestsTextEditor({
  detail,
  live = detail,
  projectRef,
  field,
  baseUrl,
  children,
}: PullRequestsEditorProps & { field: "title" | "body"; children: ReactNode }) {
  const draftKey = field === "title" ? "titleEdit" : "bodyEdit";
  const saved = usePullRequestsStore((s) => s.selectDraft(projectRef, detail.number)[draftKey]);
  const { run, pending, requestKind } = usePullRequestsActions();
  const [editing, setEditing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState("write");
  const busy = useRef(false);
  const value = saved ?? detail[field];
  const label = field === "title" ? "Title" : "Description";
  const permission = constrainPermission(
    detail.permissions.editPullRequest,
    live === null ? "Loading…" : pending || saving ? "Wait for the current action to finish" : null,
  );
  const savePermission = constrainPermission(
    permission,
    field === "title" && !value.trim() ? "Enter a title" : null,
  );
  function setDraft(next: string | null) {
    const store = usePullRequestsStore.getState();
    if (field === "title") store.setTitleEdit(projectRef, detail.number, next);
    else store.setBodyEdit(projectRef, detail.number, next);
  }
  async function save() {
    if (!savePermission.allowed || busy.current) return;
    busy.current = true;
    setSaving(true);
    setError(null);
    try {
      await run({
        action: "editPullRequest",
        title: null,
        body: null,
        baseBranch: null,
        [field]: value,
      });
      if (
        usePullRequestsStore.getState().selectDraft(projectRef, detail.number)[draftKey] === value
      )
        setDraft(null);
      setEditing(false);
    } catch (cause) {
      setError(pullRequestsActionError(cause, requestKind));
    } finally {
      busy.current = false;
      setSaving(false);
    }
  }
  if (!editing)
    return (
      <div className={field === "title" ? "flex min-w-0 items-start gap-2" : "space-y-2"}>
        {field === "title" ? children : null}
        <div className="flex justify-end">
          <PermissionButton
            mutation
            permission={permission}
            variant="ghost"
            size={field === "title" ? "icon-sm" : "sm"}
            aria-label={`Edit ${label.toLowerCase()}`}
            onClick={() => {
              if (saved === null) setDraft(detail[field]);
              setEditing(true);
              setError(null);
            }}
          >
            <PencilIcon aria-hidden="true" />
            {field === "body" ? "Edit" : null}
          </PermissionButton>
        </div>
        {field === "body" ? children : null}
      </div>
    );
  return (
    <section
      aria-label={`Edit ${label.toLowerCase()}`}
      className="w-full space-y-2"
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing) return;
        if (event.key === "Escape" && !saving) {
          event.preventDefault();
          setEditing(false);
        } else if (event.key === "Enter" && (field === "title" || event.ctrlKey || event.metaKey)) {
          event.preventDefault();
          void save();
        }
      }}
    >
      {field === "title" ? (
        <Input
          aria-label={label}
          autoFocus
          value={value}
          readOnly={saving}
          onChange={(event) => setDraft(event.target.value)}
        />
      ) : (
        <Tabs value={tab} onValueChange={(next) => setTab(String(next))}>
          <TabsList aria-label="Description editor">
            <TabsTab value="write">Write</TabsTab>
            <TabsTab value="preview">Preview</TabsTab>
          </TabsList>
          <TabsPanel value="write">
            <textarea
              autoFocus
              aria-label={label}
              className="min-h-40 w-full resize-y rounded-md border border-border bg-background p-3 text-sm"
              value={value}
              readOnly={saving}
              onChange={(event) => setDraft(event.target.value)}
            />
          </TabsPanel>
          <TabsPanel
            value="preview"
            className="min-h-24 rounded-md border border-border p-3"
            data-text-surface="background"
          >
            {value ? (
              <PullRequestsMarkdown text={value} {...(baseUrl ? { baseUrl } : {})} />
            ) : (
              <p className="text-sm text-muted-foreground">Nothing to preview</p>
            )}
          </TabsPanel>
        </Tabs>
      )}
      {error ? (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      ) : null}
      <div className="flex justify-end gap-2">
        <Button variant="outline" size="sm" disabled={saving} onClick={() => setEditing(false)}>
          Cancel
        </Button>
        <PermissionButton
          mutation
          permission={savePermission}
          size="sm"
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save"}
        </PermissionButton>
      </div>
    </section>
  );
}
