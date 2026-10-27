import type { PullRequestsPermission } from "@coddy/contracts";
import type { PullRequestsScope } from "../usePullRequestsAction";
import { PencilIcon } from "lucide-react";
import { useContext, useId, useRef, useState } from "react";
import { Dialog, DialogPopup, DialogTitle, DialogHeader, DialogFooter } from "../../ui/dialog";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
import { PullRequestsLabelChip } from "../shared/PullRequestsLabelChip";
import { MutationsDisabledContext } from "../../ui/mutationAvailability";
import { pullRequestsActionError, usePullRequestsActions } from "../usePullRequestsAction";
import { usePullRequestsVocabulary } from "./usePullRequestsVocabulary";
export interface PullRequestsPickerProps {
  scope: PullRequestsScope;
  label: string;
  kind: "labels" | "users" | "milestones";
  multiple: boolean;
  selected: readonly string[];
  permission: PullRequestsPermission;
  exclude?: readonly string[];
  onChange: (add: string[], remove: string[]) => Promise<unknown>;
}
function PickerDialog({ onClose, ...props }: PullRequestsPickerProps & { onClose: () => void }) {
  const { requestKind } = usePullRequestsActions();
  const query = usePullRequestsVocabulary(props.scope, props.kind);
  const name = useId();
  const source = props.selected;
  const [selection, setSelection] = useState({ source, values: props.selected });
  if (source !== selection.source) setSelection({ source, values: props.selected });
  const selected = source === selection.source ? selection.values : props.selected;
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busy = useRef(false);
  const permission = constrainPermission(
    props.permission,
    pending ? "Wait for the current action to finish" : null,
  );
  const entryValue = (entry: { id: string; label: string }) =>
    props.kind === "milestones" ? entry.id : entry.label;
  const entries = [...query.entries];
  for (const value of selected)
    if (
      !entries.some((entry) => entryValue(entry) === value) &&
      value.toLocaleLowerCase().includes(query.search.toLocaleLowerCase())
    )
      entries.push({ id: value, label: value, color: null, description: null });
  const choices = entries.filter((entry) => !props.exclude?.includes(entryValue(entry)));
  async function change(value: string | null) {
    if (busy.current || !permission.allowed) return;
    const values =
      value === null
        ? []
        : props.multiple
          ? selected.includes(value)
            ? selected.filter((v) => v !== value)
            : [...selected, value]
          : [value];
    const add = values.filter((v) => !selected.includes(v));
    const remove = selected.filter((v) => !values.includes(v));
    if (!add.length && !remove.length) return;
    busy.current = true;
    setPending(true);
    setError(null);
    try {
      await props.onChange(add, remove);
      setSelection({ source, values });
    } catch (cause) {
      setError(pullRequestsActionError(cause, requestKind));
    } finally {
      busy.current = false;
      setPending(false);
    }
  }
  return (
    <Dialog
      open
      onOpenChange={(open) => {
        if (!open && !busy.current) onClose();
      }}
    >
      <DialogPopup className="sm:max-w-md" data-text-surface="popover">
        <DialogHeader>
          <DialogTitle>Edit {props.label}</DialogTitle>
        </DialogHeader>
        <Input
          autoFocus
          aria-label={`Search ${props.label}`}
          placeholder={`Search ${props.label.toLowerCase()}`}
          value={query.search}
          onChange={(event) => query.onSearch(event.target.value)}
        />
        <div className="max-h-80 overflow-y-auto" data-text-surface="popover">
          {choices.map((entry) => {
            const value = entryValue(entry);
            return (
              <label
                key={value}
                className="flex min-h-10 cursor-pointer items-center gap-3 rounded-md px-2 py-2 hover:bg-accent"
                title={permission.reason ?? entry.description ?? undefined}
              >
                <input
                  type={props.multiple ? "checkbox" : "radio"}
                  name={name}
                  aria-label={entry.label}
                  checked={selected.includes(value)}
                  disabled={!permission.allowed}
                  onChange={() => void change(value)}
                />
                {props.kind === "labels" ? (
                  <PullRequestsLabelChip
                    label={{
                      name: entry.label,
                      color: entry.color,
                      description: entry.description,
                    }}
                  />
                ) : (
                  <span className="text-sm">{entry.label}</span>
                )}
              </label>
            );
          })}
          {query.searching ? (
            <p role="status" className="p-2 text-sm">
              Loading…
            </p>
          ) : null}
          {!choices.length && !query.searching && !query.error ? (
            <p className="p-2 text-sm text-muted-foreground">No matches</p>
          ) : null}
        </div>
        {query.error ? (
          <div>
            <p role="alert" className="text-sm text-destructive">
              {query.error}
            </p>
            <Button size="sm" variant="outline" onClick={query.refresh}>
              Retry
            </Button>
          </div>
        ) : null}
        {error ? (
          <p role="alert" className="text-sm text-destructive">
            {error}
          </p>
        ) : null}
        {!permission.allowed ? (
          <p className="text-xs text-muted-foreground">{permission.reason}</p>
        ) : null}
        <DialogFooter>
          {!props.multiple ? (
            <PermissionButton
              mutation
              permission={constrainPermission(
                permission,
                selected.length ? null : "No milestone selected",
              )}
              variant="outline"
              onClick={() => void change(null)}
            >
              Clear
            </PermissionButton>
          ) : null}
          <Button disabled={pending} onClick={onClose}>
            Done
          </Button>
        </DialogFooter>
      </DialogPopup>
    </Dialog>
  );
}
export function PullRequestsPicker(props: PullRequestsPickerProps) {
  const [open, setOpen] = useState(false);
  const disabledReason = useContext(MutationsDisabledContext);
  const permission = constrainPermission(props.permission, disabledReason);
  return (
    <>
      <PermissionButton
        mutation
        permission={permission}
        aria-label={`Edit ${props.label}`}
        variant="ghost"
        size="icon-sm"
        onClick={() => setOpen(true)}
      >
        <PencilIcon aria-hidden="true" />
      </PermissionButton>
      {open ? (
        <PickerDialog {...props} permission={permission} onClose={() => setOpen(false)} />
      ) : null}
    </>
  );
}
