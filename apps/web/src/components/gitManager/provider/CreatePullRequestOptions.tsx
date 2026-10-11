import type { EnvironmentId, PullRequestsCreateDefaults } from "@bibcode/contracts";
import { memo, type ReactNode, useId, useMemo } from "react";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Checkbox } from "~/components/ui/checkbox";
import {
  Combobox,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
  ComboboxPopup,
} from "~/components/ui/combobox";
import { Label } from "~/components/ui/label";
import { usePullRequestsVocabulary } from "../../pullRequests/edit/usePullRequestsVocabulary";
import { PullRequestsLabelChip } from "../../pullRequests/shared/PullRequestsLabelChip";
import {
  type CreateOptionsState,
  squashControl,
  type VocabularyChoice,
} from "./GitManagerPullRequestPanel.logic";

type OptionScope = { readonly environmentId: EnvironmentId; readonly cwd: string };

interface OptionPickerProps {
  readonly scope: OptionScope;
  readonly label: string;
  readonly kind: "users" | "labels" | "milestones";
  readonly multiple: boolean;
  readonly value: readonly VocabularyChoice[];
  readonly onChange: (value: VocabularyChoice[]) => void;
  readonly disabled: boolean;
  readonly action?: ReactNode;
}

/** A searchable host-vocabulary picker; a failed read leaves creation available. */
function OptionPicker(props: OptionPickerProps) {
  const id = useId();
  const query = usePullRequestsVocabulary(props.scope, props.kind);
  const items = useMemo(() => query.entries.map((entry) => entry.id), [query.entries]);
  const byId = useMemo(() => {
    const entries = new Map<string, { label: string; color: string | null }>(
      query.entries.map((entry) => [entry.id, { label: entry.label, color: entry.color }]),
    );
    // Keep chosen entries resolvable while a search narrows the list.
    for (const choice of props.value) {
      if (!entries.has(choice.id)) entries.set(choice.id, { label: choice.label, color: null });
    }
    return entries;
  }, [query.entries, props.value]);
  const selectedIds = props.value.map((choice) => choice.id);
  const choose = (ids: readonly string[]) =>
    props.onChange(
      ids.flatMap((choiceId) => {
        const entry = byId.get(choiceId);
        return entry === undefined ? [] : [{ id: choiceId, label: entry.label }];
      }),
    );
  return (
    <div className="grid gap-1.5">
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id}>{props.label}</Label>
        {props.action}
      </div>
      <Combobox
        items={items}
        filter={null}
        multiple={props.multiple}
        value={props.multiple ? selectedIds : (selectedIds[0] ?? null)}
        // A single choice shows its label in the input; several show as chips below.
        itemToStringLabel={(itemId: string) => byId.get(itemId)?.label ?? itemId}
        {...(props.multiple ? { inputValue: query.search } : {})}
        onInputValueChange={(value, details) => {
          if (props.multiple || details.reason === "input-change") query.onSearch(value);
        }}
        onValueChange={(value: string | string[] | null) => {
          // A made or cleared single choice ends that search.
          if (!props.multiple) query.onSearch("");
          choose(value === null ? [] : Array.isArray(value) ? value : [value]);
        }}
        disabled={props.disabled}
      >
        <ComboboxInput id={id} placeholder={props.multiple ? "Search" : "None"} showClear />
        <ComboboxPopup>
          <ComboboxList data-text-surface="popover">
            {(entryId: string) => (
              <ComboboxItem key={entryId} value={entryId}>
                {byId.get(entryId)?.label ?? entryId}
              </ComboboxItem>
            )}
          </ComboboxList>
          {query.searching ? (
            <p role="status" className="p-2 text-sm">
              Loading…
            </p>
          ) : null}
        </ComboboxPopup>
      </Combobox>
      {query.error ? (
        <div className="flex items-center gap-2 text-sm">
          <p role="alert" className="text-destructive">
            Couldn't load {props.label.toLowerCase()}.
          </p>
          <Button size="sm" variant="outline" onClick={query.refresh}>
            Retry
          </Button>
        </div>
      ) : null}
      {props.multiple && props.value.length > 0 ? (
        <div className="flex flex-wrap gap-1">
          {props.value.map((choice) =>
            props.kind === "labels" ? (
              <PullRequestsLabelChip
                key={choice.id}
                label={{
                  name: choice.label,
                  color: byId.get(choice.id)?.color ?? null,
                  description: null,
                }}
              />
            ) : (
              <Badge key={choice.id} variant="outline" size="sm">
                {choice.label}
              </Badge>
            ),
          )}
        </div>
      ) : null}
    </div>
  );
}

/**
 * Assignee, reviewer, milestone and labels for both providers, plus GitLab's merge options.
 * GitLab takes one assignee and one reviewer; GitHub takes several.
 */
export const CreatePullRequestOptions = memo(function CreatePullRequestOptions(props: {
  readonly scope: OptionScope;
  readonly providerKind: string | null;
  readonly defaults: PullRequestsCreateDefaults | null;
  /** Until GitLab's settings are known, untouched merge boxes leave the choice to the project. */
  readonly defaultsState: "ready" | "loading" | "error";
  readonly onRetryDefaults: () => void;
  readonly value: CreateOptionsState;
  readonly onChange: (value: CreateOptionsState) => void;
  readonly disabled: boolean;
}) {
  const gitlab = props.providerKind === "gitlab";
  const severalPeople = !gitlab;
  const set = (patch: Partial<CreateOptionsState>) => props.onChange({ ...props.value, ...patch });
  const squash = squashControl(props.defaults);
  const projectDecides = props.defaultsState !== "ready";
  // Untouched boxes whose project setting is unknown leave the choice to GitLab.
  const removeUnknown = projectDecides || props.defaults?.removeSourceBranch == null;
  const squashUnknown = projectDecides || props.defaults?.squash == null;
  const viewer = props.defaults?.viewer ?? null;
  const assignedToViewer =
    viewer !== null && props.value.assignees.some((choice) => choice.id === viewer.id);
  return (
    <div className="grid gap-3">
      {props.defaultsState === "error" ? (
        <div className="flex items-center gap-2 text-sm">
          <p role="alert" className="text-destructive">
            {gitlab
              ? "Couldn't load your account and project settings."
              : "Couldn't load your account settings."}
          </p>
          <Button size="sm" variant="outline" onClick={props.onRetryDefaults}>
            Retry
          </Button>
        </div>
      ) : null}
      <OptionPicker
        scope={props.scope}
        label="Assignee"
        kind="users"
        multiple={severalPeople}
        value={props.value.assignees}
        onChange={(assignees) => set({ assignees })}
        disabled={props.disabled}
        action={
          viewer === null ? null : (
            <Button
              size="xs"
              variant="link"
              disabled={props.disabled || assignedToViewer}
              onClick={() =>
                set({
                  assignees: severalPeople ? [...props.value.assignees, viewer] : [viewer],
                })
              }
            >
              Assign to me
            </Button>
          )
        }
      />
      <OptionPicker
        scope={props.scope}
        label="Reviewer"
        kind="users"
        multiple={severalPeople}
        value={props.value.reviewers}
        onChange={(reviewers) => set({ reviewers })}
        disabled={props.disabled}
      />
      <OptionPicker
        scope={props.scope}
        label="Milestone"
        kind="milestones"
        multiple={false}
        value={props.value.milestone === null ? [] : [props.value.milestone]}
        onChange={(chosen) => set({ milestone: chosen[0] ?? null })}
        disabled={props.disabled}
      />
      <OptionPicker
        scope={props.scope}
        label="Labels"
        kind="labels"
        multiple
        value={props.value.labels}
        onChange={(labels) => set({ labels })}
        disabled={props.disabled}
      />
      {gitlab ? (
        <fieldset className="grid gap-2">
          <legend className="text-sm font-medium">Merge options</legend>
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={
                props.value.removeSourceBranch ?? props.defaults?.removeSourceBranch ?? false
              }
              indeterminate={removeUnknown && props.value.removeSourceBranch === null}
              onCheckedChange={(checked) => set({ removeSourceBranch: checked })}
              disabled={props.disabled}
            />
            Delete source branch when merge request is accepted.
          </label>
          <label className="flex items-center gap-2 text-sm">
            <Checkbox
              checked={squash.locked ? squash.checked : (props.value.squash ?? squash.checked)}
              indeterminate={squashUnknown && props.value.squash === null}
              onCheckedChange={(checked) => set({ squash: checked })}
              disabled={props.disabled || squash.locked}
            />
            Squash commits when merge request is accepted.
          </label>
          {props.defaultsState === "loading" ? (
            <p className="text-xs text-muted-foreground">Loading the project's merge settings…</p>
          ) : squash.note !== null ? (
            <p className="text-xs text-muted-foreground">{squash.note}</p>
          ) : removeUnknown || squashUnknown ? (
            <p className="text-xs text-muted-foreground">
              Untouched options follow the project's settings.
            </p>
          ) : null}
        </fieldset>
      ) : null}
    </div>
  );
});
