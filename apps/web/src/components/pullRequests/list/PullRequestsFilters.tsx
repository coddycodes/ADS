import { resolveChangeRequestPresentationForKind } from "@coddy/shared/sourceControl";
import type {
  EnvironmentId,
  PullRequestsContext,
  PullRequestsListInput,
  PullRequestsVocabularyInput,
} from "@coddy/contracts";
import { useDebouncer } from "@tanstack/react-pacer";
import { useMemo, useState } from "react";
import type { PullRequestsFilters as Filters } from "../../../pullRequestsStore";
import { pullRequestsEnvironment } from "../../../state/pullRequests";
import { usePullRequestsQuery } from "../shared/usePullRequestsQuery";
import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import {
  Menu,
  MenuCheckboxItem,
  MenuItem,
  MenuPopup,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "../../ui/menu";

type Scope = { environmentId: EnvironmentId; cwd: string };
type Option = { value: string; label: string };
const DRAFT_OPTIONS: Option[] = [
  { value: "", label: "All" },
  { value: "only", label: "Drafts" },
  { value: "exclude", label: "Ready" },
];
const SORT_OPTIONS: Option[] = [
  { value: "newest", label: "Newest" },
  { value: "oldest", label: "Oldest" },
  { value: "recently_updated", label: "Recently updated" },
  { value: "most_commented", label: "Most commented" },
];
const REVIEW_OPTIONS: Option[] = [
  { value: "", label: "Any review status" },
  { value: "review_required", label: "Review required" },
  { value: "approved", label: "Approved" },
  { value: "changes_requested", label: "Changes requested" },
];
const APPROVAL_OPTIONS: Option[] = [
  { value: "", label: "Any review status" },
  { value: "approved", label: "Approved" },
  { value: "not_approved", label: "Not approved" },
];
function ChoiceMenu({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: readonly Option[];
  onChange: (value: string) => void;
}) {
  const selected = options.find((o) => o.value === value)?.label;
  return (
    <Menu>
      <MenuTrigger render={<Button size="sm" variant="outline" aria-label={label} />}>
        {label}
        {value && selected ? `: ${selected}` : ""}
      </MenuTrigger>
      <MenuPopup align="start">
        <MenuRadioGroup value={value} onValueChange={onChange}>
          {options.map((o) => (
            <MenuRadioItem key={o.value} value={o.value}>
              {o.label}
            </MenuRadioItem>
          ))}
        </MenuRadioGroup>
      </MenuPopup>
    </Menu>
  );
}
function ActorFilter({
  label,
  value,
  onChange,
}: {
  label: string;
  value: string | null;
  onChange: (value: string | null) => void;
}) {
  const [custom, setCustom] = useState(value !== null && value !== "@me");
  return (
    <div className="flex items-center gap-1">
      <ChoiceMenu
        label={label}
        value={custom ? "custom" : (value ?? "")}
        options={[
          { value: "", label: "Anyone" },
          { value: "@me", label: "Me" },
          { value: "custom", label: value && value !== "@me" ? value : "Custom login…" },
        ]}
        onChange={(next) => {
          setCustom(next === "custom");
          onChange(next === "custom" || next === "" ? null : next);
        }}
      />
      {custom ? (
        <Input
          className="h-8 w-32 text-xs"
          aria-label={`${label} login`}
          placeholder="Login"
          value={value ?? ""}
          onChange={(e) => onChange(e.target.value || null)}
        />
      ) : null}
    </div>
  );
}
function SearchFilter({
  value,
  label,
  onChange,
}: {
  value: string | null;
  label: string;
  onChange: (value: string) => void;
}) {
  const [edit, setEdit] = useState({ source: value, text: value ?? "" });
  const draft = edit.source === value ? edit.text : (value ?? "");
  const debouncer = useDebouncer(onChange, { wait: 300 });
  return (
    <Input
      aria-label={label}
      className="h-8 min-w-40 flex-1 text-xs"
      placeholder={label}
      value={draft}
      onChange={(e) => {
        setEdit({ source: value, text: e.target.value });
        debouncer.maybeExecute(e.target.value);
      }}
      onKeyDown={(event) => {
        if (event.key === "Enter" && !event.nativeEvent.isComposing) {
          event.preventDefault();
          debouncer.cancel();
          onChange(draft);
        }
      }}
    />
  );
}
function VocabularyPicker({
  scope,
  kind,
  label,
  selected,
  multiple = false,
  onChange,
}: {
  scope: Scope;
  kind: PullRequestsVocabularyInput["kind"];
  label: string;
  selected: readonly string[];
  multiple?: boolean;
  onChange: (values: string[]) => void;
}) {
  const [requested, setRequested] = useState(false);
  const atom = useMemo(
    () =>
      requested
        ? pullRequestsEnvironment.getVocabulary({
            environmentId: scope.environmentId,
            input: { cwd: scope.cwd, kind, query: null },
          })
        : null,
    [kind, requested, scope.cwd, scope.environmentId],
  );
  const query = usePullRequestsQuery(atom);
  return (
    <Menu
      onOpenChange={(open) => {
        if (open) setRequested(true);
      }}
    >
      <MenuTrigger render={<Button size="sm" variant="outline" aria-label={label} />}>
        {label}
        {selected.length ? `: ${selected.join(", ")}` : ""}
      </MenuTrigger>
      <MenuPopup align="start" className="max-w-80">
        <MenuItem onClick={() => onChange([])}>{multiple ? "Clear selection" : "Any"}</MenuItem>
        {query.isPending ? (
          <p role="status" className="px-2 py-1 text-xs text-muted-foreground">
            Loading…
          </p>
        ) : null}
        {query.error ? (
          <div className="space-y-2 p-2 text-xs">
            <p role="alert">{query.error}</p>
            <Button variant="outline" size="xs" onClick={query.refresh}>
              Retry
            </Button>
          </div>
        ) : null}
        {query.data?.entries.map((entry) =>
          multiple ? (
            <MenuCheckboxItem
              key={entry.id}
              checked={selected.includes(entry.label)}
              closeOnClick={false}
              onCheckedChange={(checked) =>
                onChange(
                  checked ? [...selected, entry.label] : selected.filter((v) => v !== entry.label),
                )
              }
            >
              {entry.label}
            </MenuCheckboxItem>
          ) : (
            <MenuItem key={entry.id} onClick={() => onChange([entry.label])}>
              {entry.label}
            </MenuItem>
          ),
        )}
        {query.data?.entries.length === 0 && !query.isPending && !query.error ? (
          <p className="p-2 text-xs text-muted-foreground">No {label.toLowerCase()} available.</p>
        ) : null}
        {query.data?.truncated ? (
          <p className="p-2 text-xs text-muted-foreground">Only the first results are shown.</p>
        ) : null}
      </MenuPopup>
    </Menu>
  );
}
export interface PullRequestsFiltersProps {
  scope: Scope;
  context: Extract<PullRequestsContext, { status: "available" }>;
  filters: Filters;
  sort: PullRequestsListInput["sort"];
  resetKey: number;
  onFiltersChange: (patch: Partial<Filters>) => void;
  onSortChange: (sort: PullRequestsListInput["sort"]) => void;
  onClear: () => void;
}
export function PullRequestsFilters({
  scope,
  context,
  filters,
  sort,
  resetKey,
  onFiltersChange,
  onSortChange,
  onClear,
}: PullRequestsFiltersProps) {
  const vocabulary = context.capabilities.vocabulary;
  const presentation = resolveChangeRequestPresentationForKind(context.provider);
  return (
    <div className="space-y-2 border-b border-border px-4 py-3">
      <div className="flex flex-wrap gap-2">
        <SearchFilter
          key={resetKey}
          value={filters.search}
          label={`Search ${presentation.pluralLongName}`}
          onChange={(search) => onFiltersChange({ search: search.trim() || null })}
        />
        <ChoiceMenu
          label="Sort"
          value={sort}
          options={SORT_OPTIONS}
          onChange={(value) => onSortChange(value as PullRequestsListInput["sort"])}
        />
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <ActorFilter
          key={`author:${resetKey}`}
          label="Author"
          value={filters.author}
          onChange={(author) => onFiltersChange({ author })}
        />
        <ActorFilter
          key={`assignee:${resetKey}`}
          label="Assignee"
          value={filters.assignee}
          onChange={(assignee) => onFiltersChange({ assignee })}
        />
        <ActorFilter
          key={`reviewer:${resetKey}`}
          label={vocabulary.reviewer}
          value={filters.reviewer}
          onChange={(reviewer) => onFiltersChange({ reviewer })}
        />
        <ChoiceMenu
          label="Review status"
          value={filters.reviewStatus ?? ""}
          options={context.capabilities.closedTabIncludesMerged ? REVIEW_OPTIONS : APPROVAL_OPTIONS}
          onChange={(value) =>
            onFiltersChange({ reviewStatus: (value || null) as Filters["reviewStatus"] })
          }
        />
        <ChoiceMenu
          label="Draft"
          value={filters.draft ?? ""}
          options={DRAFT_OPTIONS}
          onChange={(value) => onFiltersChange({ draft: (value || null) as Filters["draft"] })}
        />
        <VocabularyPicker
          scope={scope}
          kind="labels"
          label="Labels"
          multiple
          selected={filters.labels}
          onChange={(labels) => onFiltersChange({ labels })}
        />
        <VocabularyPicker
          scope={scope}
          kind="milestones"
          label="Milestone"
          selected={filters.milestone ? [filters.milestone] : []}
          onChange={(values) => onFiltersChange({ milestone: values[0] ?? null })}
        />
        <VocabularyPicker
          scope={scope}
          kind="branches"
          label="Target branch"
          selected={filters.targetBranch ? [filters.targetBranch] : []}
          onChange={(values) => onFiltersChange({ targetBranch: values[0] ?? null })}
        />
        <Button size="sm" variant="ghost" onClick={onClear}>
          Clear filters
        </Button>
      </div>
      {context.capabilities.closedTabIncludesMerged && filters.search ? (
        <p className="text-xs text-muted-foreground">Text search has a separate host rate limit.</p>
      ) : null}
    </div>
  );
}
