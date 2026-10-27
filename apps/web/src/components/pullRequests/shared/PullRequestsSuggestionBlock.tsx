import type { PullRequestsPermission, PullRequestsTimelineItem } from "@coddy/contracts";
import { createContext, memo, useContext, useRef, useState } from "react";
import { writeTextToClipboard } from "../../../hooks/useCopyToClipboard";
import { Button } from "../../ui/button";
import { Popover, PopoverPopup, PopoverTitle, PopoverTrigger } from "../../ui/popover";
import { usePullRequestsActions } from "../usePullRequestsAction";
import { PermissionButton, constrainPermission } from "../../ui/permission-button";
type Suggestion = NonNullable<
  Extract<PullRequestsTimelineItem, { kind: "thread" }>["comments"][number]["suggestion"]
>;
export const PullRequestsSuggestionSelectionContext = createContext<{
  selected: ReadonlySet<string>;
  toggle: (id: string, checked: boolean) => void;
} | null>(null);
export function PullRequestsApplySuggestionsButton({
  suggestionIds,
  permission,
  label,
  onApplied,
  live = true,
}: {
  suggestionIds: readonly string[];
  permission: PullRequestsPermission;
  label: string;
  onApplied?: (ids: readonly string[]) => void;
  /**
   * Applying a suggestion only ever acts on a live detail, never a
   * snapshot. Defaults to `true` so callers that have not adopted the split
   * keep acting as before.
   */
  live?: boolean;
}) {
  const { run, pending } = usePullRequestsActions();
  const [open, setOpen] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [applying, setApplying] = useState(false);
  const busy = useRef(false);
  const available = constrainPermission(
    permission,
    !live
      ? "Loading…"
      : pending || applying
        ? "Wait for the current action to finish"
        : suggestionIds.length === 0
          ? "This suggestion cannot be applied to the current head"
          : null,
  );
  async function apply() {
    if (!available.allowed || busy.current) return;
    busy.current = true;
    setApplying(true);
    setError(null);
    const ids = [...suggestionIds];
    try {
      await run({
        action: "applySuggestions",
        suggestionIds: ids,
        commitMessage: message.trim() ? message : null,
      });
      setOpen(false);
      setMessage("");
      onApplied?.(ids);
    } catch (cause) {
      setError(
        cause instanceof Error
          ? cause.message
          : "Could not apply the suggestion. Refresh and try again.",
      );
    } finally {
      busy.current = false;
      setApplying(false);
    }
  }
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverTrigger
        disabled={!available.allowed}
        render={<PermissionButton mutation permission={available} variant="outline" size="sm" />}
      >
        {label}
      </PopoverTrigger>
      {open ? (
        <PopoverPopup
          align="end"
          className="w-[min(24rem,calc(100vw-2rem))]"
          data-text-surface="popover"
        >
          <div className="space-y-3">
            <PopoverTitle>Apply suggested changes</PopoverTitle>
            <label className="block space-y-2 text-sm">
              <span>Commit message (optional)</span>
              <input
                aria-label="Commit message (optional)"
                value={message}
                readOnly={pending || applying}
                onChange={(event) => setMessage(event.currentTarget.value)}
                className="w-full rounded-md border border-border bg-background px-3 py-2"
              />
            </label>
            {error ? (
              <p role="alert" className="text-sm text-destructive">
                {error}
              </p>
            ) : null}
            <div className="flex justify-end gap-2">
              <Button variant="outline" size="sm" onClick={() => setOpen(false)}>
                Cancel
              </Button>
              <PermissionButton
                mutation
                permission={available}
                size="sm"
                onClick={() => void apply()}
              >
                {applying
                  ? "Applying…"
                  : suggestionIds.length === 1
                    ? "Apply suggestion"
                    : `Apply ${suggestionIds.length} suggestions`}
              </PermissionButton>
            </div>
          </div>
        </PopoverPopup>
      ) : null}
    </Popover>
  );
}
export const PullRequestsSuggestionBlock = memo(function PullRequestsSuggestionBlock({
  suggestion,
  permission,
  live = true,
}: {
  suggestion: Suggestion;
  permission: PullRequestsPermission;
  /**
   * Applying a suggestion only ever acts on a live detail, never a
   * snapshot. Defaults to `true` so callers that have not adopted the split
   * keep acting as before.
   */
  live?: boolean;
}) {
  const selection = useContext(PullRequestsSuggestionSelectionContext);
  const [copyStatus, setCopyStatus] = useState<string | null>(null);
  const available = constrainPermission(
    permission,
    !live
      ? "Loading…"
      : !suggestion.applicable || suggestion.id === null
        ? "This suggestion cannot be applied to the current head"
        : null,
  );
  return (
    <section
      aria-label="Suggested change"
      className="overflow-hidden rounded-md border border-border"
    >
      <div className="flex flex-wrap items-center justify-between gap-2 bg-muted/40 px-3 py-2 text-xs">
        <span>
          Lines {suggestion.fromLine}–{suggestion.toLine}
        </span>
        {selection && suggestion.id !== null ? (
          <label
            title={available.reason ?? undefined}
            className="flex cursor-pointer items-center gap-2"
          >
            <input
              type="checkbox"
              aria-label={`Select suggestion for lines ${suggestion.fromLine}–${suggestion.toLine}`}
              disabled={!available.allowed}
              checked={selection.selected.has(suggestion.id)}
              onChange={(event) => {
                if (suggestion.id !== null)
                  selection.toggle(suggestion.id, event.currentTarget.checked);
              }}
            />
            Select
          </label>
        ) : null}
        <div className="flex gap-2">
          <PullRequestsApplySuggestionsButton
            suggestionIds={suggestion.id === null ? [] : [suggestion.id]}
            permission={available}
            label="Apply"
            live={live}
          />
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void writeTextToClipboard(suggestion.toContent, "suggestion").then(
                () => setCopyStatus("Suggestion copied"),
                () => setCopyStatus("Could not copy the suggestion. Select and copy the text."),
              );
            }}
          >
            Copy
          </Button>
        </div>
      </div>
      {!available.allowed ? (
        <p className="px-3 pt-2 text-xs text-muted-foreground">{available.reason}</p>
      ) : null}
      {copyStatus ? (
        <p role="status" className="px-3 pt-2 text-xs">
          {copyStatus}
        </p>
      ) : null}
      <pre className="overflow-auto p-3 font-mono text-xs">
        <span className="block bg-red-500/10">
          {suggestion.fromContent
            .split("\n")
            .map((line) => `-${line}`)
            .join("\n")}
        </span>
        <span className="block bg-green-500/10">
          {suggestion.toContent
            .split("\n")
            .map((line) => `+${line}`)
            .join("\n")}
        </span>
      </pre>
    </section>
  );
});
