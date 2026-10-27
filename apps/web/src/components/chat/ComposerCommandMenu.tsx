import { BotIcon } from "lucide-react";
import { memo, useLayoutEffect, useMemo, useRef } from "react";

import { formatProviderSkillInstallSource } from "~/providerSkillPresentation";
import { cn } from "~/lib/utils";
import {
  Command,
  CommandGroup,
  CommandGroupLabel,
  CommandItem,
  CommandList,
  CommandSeparator,
} from "../ui/command";
import { type ComposerCommandItem, type ComposerCommandGroupId } from "./composerCommandItems";
import { PierreEntryIcon } from "./PierreEntryIcon";

type ComposerCommandGroup = {
  id: ComposerCommandGroupId;
  label: string;
  items: ComposerCommandItem[];
};

const GROUPS = [
  ["coddy-studio", "Coddy Studio"],
  ["commands", "Commands"],
  ["skills", "Skills"],
  ["files", "Files"],
  ["agents", "Agents"],
] as const satisfies ReadonlyArray<readonly [ComposerCommandGroupId, string]>;

function SkillGlyph(props: { className?: string }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.85"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={props.className}
      aria-hidden="true"
    >
      <path d="M21 8a2 2 0 0 0-1-1.73l-7-4a2 2 0 0 0-2 0l-7 4A2 2 0 0 0 3 8v8a2 2 0 0 0 1 1.73l7 4a2 2 0 0 0 2 0l7-4A2 2 0 0 0 21 16Z" />
      <path d="m3.3 7 8.7 5 8.7-5" />
      <path d="M12 22V12" />
    </svg>
  );
}

function groupCommandItems(items: ReadonlyArray<ComposerCommandItem>): ComposerCommandGroup[] {
  return GROUPS.flatMap(([id, label]) => {
    const groupItems = items.filter((item) => item.group === id);
    return groupItems.length > 0 ? [{ id, label, items: groupItems }] : [];
  });
}

export const ComposerCommandMenu = memo(function ComposerCommandMenu(props: {
  items: ReadonlyArray<ComposerCommandItem>;
  resolvedTheme: "light" | "dark";
  isLoading: boolean;
  loadingText?: string;
  issueText?: string | null;
  onRetry?: () => void;
  emptyStateText?: string;
  activeItemId: string | null;
  onHighlightedItemChange: (itemId: string | null) => void;
  onSelect: (item: ComposerCommandItem) => void;
}) {
  const listRef = useRef<HTMLDivElement>(null);
  const groups = useMemo(() => groupCommandItems(props.items), [props.items]);

  useLayoutEffect(() => {
    if (!props.activeItemId || !listRef.current) return;
    const el = listRef.current.querySelector<HTMLElement>(
      `[data-composer-item-id="${CSS.escape(props.activeItemId)}"]`,
    );
    el?.scrollIntoView({ block: "nearest" });
  }, [props.activeItemId]);

  return (
    <Command
      autoHighlight={false}
      mode="none"
      onItemHighlighted={(highlightedValue) => {
        props.onHighlightedItemChange(
          typeof highlightedValue === "string" ? highlightedValue : null,
        );
      }}
    >
      <div
        ref={listRef}
        data-text-surface
        aria-busy={props.isLoading ? "true" : "false"}
        data-composer-menu="true"
        data-composer-menu-loading={props.isLoading ? "true" : "false"}
        className="relative w-full overflow-hidden rounded-[20px] border border-border/80 bg-popover/96 shadow-lg/8 backdrop-blur-xs"
      >
        {props.items.length > 0 ? (
          <>
            <CommandList className="max-h-72">
              {groups.map((group, groupIndex) => (
                <div key={group.id} data-composer-group={group.id}>
                  {groupIndex > 0 ? <CommandSeparator className="my-0.5" /> : null}
                  <CommandGroup>
                    <CommandGroupLabel
                      data-composer-group-label={group.id}
                      className="px-3 pt-2 pb-1 text-xs font-semibold uppercase text-muted-foreground"
                    >
                      {group.label}
                    </CommandGroupLabel>
                    {group.items.map((item) => (
                      <ComposerCommandMenuItem
                        key={item.id}
                        item={item}
                        resolvedTheme={props.resolvedTheme}
                        isActive={props.activeItemId === item.id}
                        onHighlight={props.onHighlightedItemChange}
                        onSelect={props.onSelect}
                      />
                    ))}
                  </CommandGroup>
                </div>
              ))}
            </CommandList>
            {props.isLoading ? (
              <p
                role="status"
                className="border-t border-border/60 px-3 py-1.5 text-xs text-muted-foreground"
              >
                {props.loadingText ?? "Searching workspace files..."}
              </p>
            ) : null}
          </>
        ) : props.isLoading || !props.issueText ? (
          <div className="px-5 py-3.5">
            <p className="text-muted-foreground text-xs">
              {props.isLoading
                ? (props.loadingText ?? "Searching workspace files...")
                : (props.emptyStateText ?? "No matching command.")}
            </p>
          </div>
        ) : null}
        {props.issueText ? (
          <div className="flex items-center gap-3 border-t border-border/60 px-3 py-2">
            <p role="status" className="min-w-0 flex-1 text-xs text-muted-foreground">
              {props.issueText}
            </p>
            {props.onRetry ? (
              <button
                type="button"
                className="shrink-0 rounded-md px-3 py-2 text-xs underline hover:bg-accent disabled:opacity-50"
                disabled={props.isLoading}
                onMouseDown={(event) => event.preventDefault()}
                onClick={props.onRetry}
              >
                Retry
              </button>
            ) : null}
          </div>
        ) : null}
      </div>
    </Command>
  );
});

const ComposerCommandMenuItem = memo(function ComposerCommandMenuItem(props: {
  item: ComposerCommandItem;
  resolvedTheme: "light" | "dark";
  isActive: boolean;
  onHighlight: (itemId: string | null) => void;
  onSelect: (item: ComposerCommandItem) => void;
}) {
  const skillSourceLabel =
    props.item.type === "provider-skill"
      ? formatProviderSkillInstallSource(props.item.skill)
      : null;

  return (
    <CommandItem
      value={props.item.id}
      data-composer-item-id={props.item.id}
      data-composer-item-active={props.isActive ? "true" : "false"}
      className={cn(
        "cursor-pointer select-none gap-2 hover:bg-transparent hover:text-inherit data-highlighted:bg-transparent data-highlighted:text-inherit",
        props.isActive && "bg-accent! text-accent-foreground!",
      )}
      onMouseMove={() => {
        if (!props.isActive) props.onHighlight(props.item.id);
      }}
      onMouseDown={(event) => {
        event.preventDefault();
      }}
      onClick={() => {
        props.onSelect(props.item);
      }}
    >
      {props.item.type === "file-reference" ? (
        <PierreEntryIcon
          pathValue={props.item.path}
          kind={props.item.pathKind}
          theme={props.resolvedTheme}
        />
      ) : null}
      {props.item.type === "coddy-studio-action" ? (
        <BotIcon className="size-4 shrink-0 text-muted-foreground/80" />
      ) : null}
      {props.item.type === "agent-reference" ? (
        <BotIcon className="size-4 shrink-0 text-muted-foreground/80" />
      ) : null}
      {props.item.type === "provider-command" ? (
        <span className="inline-flex size-4 shrink-0 items-center justify-center text-muted-foreground/80">
          <SkillGlyph className="size-3.5" />
        </span>
      ) : null}
      {props.item.type === "provider-skill" ? (
        <span className="inline-flex size-4 shrink-0 items-center justify-center text-muted-foreground/80">
          <SkillGlyph className="size-3.5" />
        </span>
      ) : null}
      <span className="flex min-w-0 flex-1 items-center gap-2">
        <span className="shrink-0">{props.item.label}</span>
        <span className="min-w-0 flex-1 truncate text-muted-foreground text-xs">
          {props.item.description}
        </span>
      </span>
      {skillSourceLabel ? (
        <span className="shrink-0 pl-2 text-muted-foreground text-xs">{skillSourceLabel}</span>
      ) : null}
    </CommandItem>
  );
});
