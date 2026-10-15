import type {
  EnvironmentId,
  GitManagerCoAuthor,
  GitManagerUndoCommitResult,
} from "@bibcode/contracts";
import { Settings2Icon, UserPlusIcon, XIcon } from "lucide-react";
import {
  memo,
  type ChangeEvent,
  type FormEvent,
  type KeyboardEvent,
  useCallback,
  useRef,
  useState,
} from "react";

import { Button } from "~/components/ui/button";
import { Checkbox } from "~/components/ui/checkbox";
import { Input } from "~/components/ui/input";
import { Popover, PopoverPopup, PopoverTrigger } from "~/components/ui/popover";
import { Textarea } from "~/components/ui/textarea";
import { useSourceControlDraft } from "~/sourceControlDraft";

import {
  buildCommitMessage,
  buildPlaceholderSummary,
  isCommitEnabled,
  isSummaryOverIdealLength,
} from "./commitBox.logic";
import { GitManagerUndoCommitStrip } from "./GitManagerUndoCommitStrip";

export interface GitManagerCommitSubmission {
  readonly message: string;
  readonly summary: string;
  readonly description: string;
  readonly coAuthors: ReadonlyArray<GitManagerCoAuthor>;
  readonly noVerify: boolean;
  readonly signoff: boolean;
  readonly allowEmpty: boolean;
  readonly amend: boolean;
}

export interface GitManagerCommitBoxProps {
  readonly scope: { readonly environmentId: EnvironmentId; readonly cwd: string };
  readonly branch: string;
  readonly includedPaths: ReadonlyArray<string>;
  readonly isBusy: boolean;
  readonly disabledReason: string | null;
  readonly onCommit: (input: GitManagerCommitSubmission) => Promise<void>;
  readonly latestCommit?: {
    readonly committedAtMs: number;
    readonly isMerge: boolean;
  } | null;
  readonly workingTreeDirty?: boolean;
  readonly onUndo?: () => Promise<GitManagerUndoCommitResult | null>;
}

function splitDraftMessage(message: string): { summary: string; description: string } {
  const newline = message.indexOf("\n");
  if (newline < 0) return { summary: message, description: "" };
  const remainder = message.slice(newline + 1);
  return {
    summary: message.slice(0, newline),
    description: remainder.startsWith("\n") ? remainder.slice(1) : remainder,
  };
}

function draftMessage(summary: string, description: string): string {
  return description.length > 0 ? `${summary}\n\n${description}` : summary;
}

function parseCoAuthor(value: string): GitManagerCoAuthor | null {
  const match = /^(.+?)\s*<([^<>]+)>$/.exec(value.trim());
  if (match === null) return null;
  const name = match[1]!.trim();
  const email = match[2]!.trim();
  return name.length > 0 && email.length > 0 ? { name, email } : null;
}

export const GitManagerCommitBox = memo(function GitManagerCommitBox({
  scope,
  branch,
  includedPaths,
  isBusy,
  disabledReason,
  onCommit,
  latestCommit = null,
  workingTreeDirty = false,
  onUndo,
}: GitManagerCommitBoxProps) {
  const { message, setMessage, clear: clearDraft } = useSourceControlDraft(scope);
  const [coAuthors, setCoAuthors] = useState<ReadonlyArray<GitManagerCoAuthor>>([]);
  const [coAuthorInput, setCoAuthorInput] = useState("");
  const [coAuthorError, setCoAuthorError] = useState<string | null>(null);
  const [noVerify, setNoVerify] = useState(false);
  const [signoff, setSignoff] = useState(false);
  const [allowEmpty, setAllowEmpty] = useState(false);
  const [isAmending, setIsAmending] = useState(false);
  const [isSubmitting, setIsSubmitting] = useState(false);
  const submittingRef = useRef(false);
  const { summary, description } = splitDraftMessage(message);
  const placeholderSummary = buildPlaceholderSummary(includedPaths);
  const busy = isBusy || isSubmitting;
  const enabled =
    disabledReason === null &&
    isCommitEnabled({
      summary,
      includedCount: includedPaths.length,
      allowEmpty,
      isAmending,
      isBusy: busy,
    });
  const disabledDescriptionId = disabledReason === null ? undefined : "git-manager-commit-disabled";

  const handleSummaryChange = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      setMessage(draftMessage(event.target.value, description));
    },
    [description, setMessage],
  );
  const handleDescriptionChange = useCallback(
    (event: ChangeEvent<HTMLTextAreaElement>) => {
      setMessage(draftMessage(summary, event.target.value));
    },
    [setMessage, summary],
  );
  const handleCoAuthorInputChange = useCallback((event: ChangeEvent<HTMLInputElement>) => {
    setCoAuthorInput(event.target.value);
    setCoAuthorError(null);
  }, []);
  const addCoAuthor = useCallback(() => {
    const coAuthor = parseCoAuthor(coAuthorInput);
    if (coAuthor === null) {
      setCoAuthorError("Enter a co-author as Name <email>.");
      return;
    }
    setCoAuthors((current) => {
      const duplicate = current.some(
        (candidate) => candidate.email.toLowerCase() === coAuthor.email.toLowerCase(),
      );
      return duplicate ? current : [...current, coAuthor];
    });
    setCoAuthorInput("");
    setCoAuthorError(null);
  }, [coAuthorInput]);
  const removeCoAuthor = useCallback((email: string) => {
    setCoAuthors((current) => current.filter((coAuthor) => coAuthor.email !== email));
  }, []);
  const toggleAmend = useCallback(() => setIsAmending((current) => !current), []);
  const submit = useCallback(async () => {
    if (!enabled || submittingRef.current) return;
    const effectiveSummary = summary.trim() || placeholderSummary || "Empty commit";
    submittingRef.current = true;
    setIsSubmitting(true);
    try {
      await onCommit({
        message: buildCommitMessage({
          summary: effectiveSummary,
          description,
          coAuthors,
        }),
        summary: effectiveSummary,
        description,
        coAuthors,
        noVerify,
        signoff,
        allowEmpty,
        amend: isAmending,
      });
      clearDraft();
      setCoAuthors([]);
      setCoAuthorInput("");
      setIsAmending(false);
    } catch {
      // The owner renders the command's typed failure verbatim. Preserve the draft.
    } finally {
      submittingRef.current = false;
      setIsSubmitting(false);
    }
  }, [
    allowEmpty,
    clearDraft,
    coAuthors,
    description,
    enabled,
    isAmending,
    noVerify,
    onCommit,
    placeholderSummary,
    signoff,
    summary,
  ]);
  const handleSubmit = useCallback(
    (event: FormEvent<HTMLFormElement>) => {
      event.preventDefault();
      void submit();
    },
    [submit],
  );
  const handleKeyDown = useCallback(
    (event: KeyboardEvent<HTMLFormElement>) => {
      if (event.key !== "Enter" || (!event.metaKey && !event.ctrlKey)) return;
      event.preventDefault();
      void submit();
    },
    [submit],
  );
  const handleUndo = useCallback(async () => {
    if (onUndo === undefined) return;
    const restored = await onUndo();
    if (restored === null) return;
    setMessage(draftMessage(restored.summary, restored.description));
    setCoAuthors(restored.coAuthors);
  }, [onUndo, setMessage]);

  return (
    <>
      <form
        aria-label="Commit Changes"
        className="shrink-0 space-y-2 border-t border-panel-separator bg-card/20 p-3"
        onKeyDown={handleKeyDown}
        onSubmit={handleSubmit}
      >
        {isAmending ? (
          <div className="flex items-center justify-between gap-3 rounded-md border border-warning/40 bg-warning/8 px-2 py-1.5 text-xs">
            <span>Amending the latest commit</span>
            <Button size="xs" variant="ghost" onClick={toggleAmend}>
              Stop Amending
            </Button>
          </div>
        ) : latestCommit !== null ? (
          <Button size="xs" variant="ghost" onClick={toggleAmend}>
            Amend Last Commit
          </Button>
        ) : null}

        <label className="block space-y-1 text-xs font-medium" htmlFor="git-manager-summary">
          Summary
          <Input
            autoComplete="off"
            id="git-manager-summary"
            name="git-manager-summary"
            placeholder={placeholderSummary || "Required summary…"}
            value={summary}
            onChange={handleSummaryChange}
          />
        </label>
        {isSummaryOverIdealLength(summary) ? (
          <p aria-live="polite" className="text-xs text-warning">
            Summary is over the ideal 50-character length.
          </p>
        ) : null}
        <label className="block space-y-1 text-xs font-medium" htmlFor="git-manager-description">
          Description
          <Textarea
            autoComplete="off"
            id="git-manager-description"
            name="git-manager-description"
            placeholder="Optional details…"
            size="sm"
            value={description}
            onChange={handleDescriptionChange}
          />
        </label>

        <div className="space-y-1">
          <label className="block text-xs font-medium" htmlFor="git-manager-co-author">
            Co-author
          </label>
          <div className="flex gap-1.5">
            <Input
              aria-describedby={coAuthorError === null ? undefined : "git-manager-co-author-error"}
              autoComplete="off"
              id="git-manager-co-author"
              name="git-manager-co-author"
              placeholder="Name <email>…"
              value={coAuthorInput}
              onChange={handleCoAuthorInputChange}
            />
            <Button
              aria-label="Add Co-author"
              size="icon-sm"
              variant="outline"
              onClick={addCoAuthor}
            >
              <UserPlusIcon aria-hidden="true" />
            </Button>
          </div>
          {coAuthorError === null ? null : (
            <p
              aria-live="polite"
              className="text-xs text-destructive"
              id="git-manager-co-author-error"
            >
              {coAuthorError}
            </p>
          )}
          {coAuthors.length === 0 ? null : (
            <ul aria-label="Co-authors" className="space-y-1">
              {coAuthors.map((coAuthor) => (
                <li
                  className="flex min-w-0 items-center gap-2 text-xs"
                  key={coAuthor.email.toLowerCase()}
                >
                  <span className="min-w-0 flex-1 truncate">
                    {coAuthor.name} &lt;{coAuthor.email}&gt;
                  </span>
                  <Button
                    aria-label={`Remove ${coAuthor.name}`}
                    size="icon-xs"
                    variant="ghost"
                    onClick={() => removeCoAuthor(coAuthor.email)}
                  >
                    <XIcon aria-hidden="true" />
                  </Button>
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="flex items-center gap-2">
          <Popover>
            <PopoverTrigger
              render={<Button aria-label="Commit Options" size="icon-sm" variant="outline" />}
            >
              <Settings2Icon aria-hidden="true" />
            </PopoverTrigger>
            <PopoverPopup align="start" className="w-64">
              <div className="space-y-2">
                <p className="text-sm font-medium">Commit Options</p>
                <label className="flex items-center gap-2 text-sm">
                  <Checkbox checked={noVerify} onCheckedChange={setNoVerify} />
                  Bypass Commit Hooks
                </label>
                <label className="flex items-center gap-2 text-sm">
                  <Checkbox checked={signoff} onCheckedChange={setSignoff} />
                  Signed-off-by
                </label>
                <label className="flex items-center gap-2 text-sm">
                  <Checkbox checked={allowEmpty} onCheckedChange={setAllowEmpty} />
                  Allow Empty
                </label>
              </div>
            </PopoverPopup>
          </Popover>
          <Button
            aria-describedby={disabledDescriptionId}
            className="min-w-0 flex-1"
            disabled={!enabled}
            title={disabledReason ?? undefined}
            type="submit"
          >
            {busy ? "Committing…" : `Commit ${includedPaths.length} files to ${branch}`}
          </Button>
        </div>
        {disabledReason === null ? null : (
          <p className="text-xs text-muted-foreground" id={disabledDescriptionId}>
            {disabledReason}
          </p>
        )}
      </form>
      {latestCommit === null || onUndo === undefined ? null : (
        <GitManagerUndoCommitStrip
          committedAtMs={latestCommit.committedAtMs}
          isAmending={isAmending}
          isBusy={busy}
          isMerge={latestCommit.isMerge}
          workingTreeDirty={workingTreeDirty}
          onUndo={handleUndo}
        />
      )}
    </>
  );
});
