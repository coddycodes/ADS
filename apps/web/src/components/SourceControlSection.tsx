import { ChevronDownIcon, MinusIcon, PlusIcon, Trash2Icon, Undo2Icon } from "lucide-react";

import { Button } from "~/components/ui/button";
import { cn } from "~/lib/utils";

import type { SourceControlListSection } from "./SourceControlChangesList";

interface SourceControlSectionHeaderProps {
  section: SourceControlListSection;
  collapsed: boolean;
  onToggleCollapsed: () => void;
}

/** A change section's title row: collapse toggle, file count and section-wide actions. */
export function SourceControlSectionHeader({
  section,
  collapsed,
  onToggleCollapsed,
}: SourceControlSectionHeaderProps) {
  const deleteAllVariant = section.discardVariant === "delete-untracked";
  return (
    <div className="group flex h-full items-center gap-1 px-1">
      <button
        type="button"
        aria-expanded={!collapsed}
        onClick={onToggleCollapsed}
        className="flex flex-1 items-center gap-1 text-xs font-medium uppercase tracking-wide text-muted-foreground"
      >
        <ChevronDownIcon
          className={cn("size-3.5 transition-transform", collapsed && "-rotate-90")}
        />
        {section.title}
        <span className="ml-1">{section.files.length}</span>
      </button>
      {section.selectionMode && section.onSelectAll ? (
        <Button
          variant="ghost"
          size="xs"
          onClick={() => section.onSelectAll?.(!section.allSelected)}
          disabled={section.disabled}
        >
          {section.allSelected ? "Clear section" : "Select all"}
        </Button>
      ) : null}
      {!section.selectionMode && section.primaryAction ? (
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label={section.primaryAction.label}
          onClick={section.primaryAction.onClick}
          disabled={section.disabled}
        >
          {section.primaryAction.icon === "stage" ? (
            <PlusIcon className="size-3.5" />
          ) : (
            <MinusIcon className="size-3.5" />
          )}
        </Button>
      ) : null}
      {!section.selectionMode && section.onDiscard ? (
        <Button
          variant="ghost"
          size="icon-xs"
          aria-label={deleteAllVariant ? "Delete all untracked" : "Discard all"}
          title={deleteAllVariant ? "Delete all untracked" : "Discard all"}
          onClick={section.onDiscard}
          disabled={section.disabled}
          className={cn(deleteAllVariant && "text-destructive hover:text-destructive")}
        >
          {deleteAllVariant ? (
            <Trash2Icon className="size-3.5" />
          ) : (
            <Undo2Icon className="size-3.5" />
          )}
        </Button>
      ) : null}
    </div>
  );
}
