import type { VcsStagingArea } from "@coddy/contracts";
import { ChevronDownIcon } from "lucide-react";
import { Fragment, useState } from "react";

import { DiffStatLabel } from "~/components/chat/DiffStatLabel";
import { Button } from "~/components/ui/button";
import { Checkbox } from "~/components/ui/checkbox";
import { Menu, MenuItem, MenuPopup, MenuSeparator } from "~/components/ui/menu";
import { cn } from "~/lib/utils";
import { workingTreeStatusBadge } from "~/sourceControlStatus";

import {
  buildSourceControlRows,
  fileCountLabel,
  folderActionTarget,
  folderCheckboxState,
  sectionItemKey,
  type SourceControlFolderGroup,
  type SourceControlListRow,
} from "./SourceControlChangesList.logic";
import { splitFilePath, type WorkingTreeFile } from "./SourceControlPanel.logic";
import {
  buildRowContextMenu,
  getRowActions,
  rowAreaOf,
  type RowActionDescriptor,
  type RowContextMenuItemId,
} from "./SourceControlRowActions.logic";
import { SourceControlSectionHeader } from "./SourceControlSection";
import { WindowedList } from "./WindowedList";

/**
 * Per-file affordances for a change row. All optional so existing call sites (and
 * legacy servers) compile unchanged; the panel wires the ones it can support.
 * Discard and delete both route through `onRequestDiscardFile` — the panel decides
 * the discard-vs-delete confirm copy from the file's area/status.
 */
export interface SourceControlRowActions {
  onStageFile?: (path: string) => void;
  onUnstageFile?: (path: string) => void;
  onRequestDiscardFile?: (file: WorkingTreeFile) => void;
  onOpenExternalEditor?: (path: string) => void;
  onCopyPath?: (path: string, relative: boolean) => void;
  onViewFile?: (path: string, area?: VcsStagingArea) => void;
  onIgnoreFileName?: (path: string) => void;
  onIgnoreParentFolder?: (path: string) => void;
}

/** One titled group of changes (Staged Changes, Changes, Untracked Files). */
export interface SourceControlListSection extends SourceControlRowActions {
  title: string;
  files: readonly WorkingTreeFile[];
  selectionMode?: boolean;
  allSelected?: boolean;
  onSelectAll?: (selected: boolean) => void;
  /**
   * When provided, a per-row stage/unstage checkbox is rendered, reflecting
   * whether the given file is currently staged. Omit this (legacy servers
   * without stage/unstage RPCs) to render no checkbox at all.
   */
  checked?: (file: WorkingTreeFile) => boolean;
  selected?: (file: WorkingTreeFile) => boolean;
  onSelect?: (path: string, selected: boolean) => void;
  /**
   * Batch form of `onSelect` used by a folder header. Omit it and the header
   * falls back to one `onSelect` call per file.
   */
  onSelectFiles?: (files: readonly WorkingTreeFile[], selected: boolean) => void;
  onToggle: (path: string) => void;
  /**
   * Batch stage/unstage used by a folder header so staging a folder costs one
   * request instead of one per file. Omit them and the header falls back to one
   * `onToggle` call per file.
   */
  onStageFiles?: (paths: readonly string[]) => void;
  onUnstageFiles?: (paths: readonly string[]) => void;
  /** Opens the file's diff; the area lets the panel pick staged vs unstaged scope. */
  onOpenFile: (path: string, area?: VcsStagingArea) => void;
  primaryAction?: { icon: "stage" | "unstage"; label: string; onClick: () => void };
  onDiscard?: () => void;
  /**
   * How the discard-all header button reads. Default "discard-all" (Undo2Icon,
   * "Discard all"); "delete-untracked" renders a destructive Trash2Icon
   * "Delete all untracked" for the untracked section.
   */
  discardVariant?: "discard-all" | "delete-untracked";
  disabled?: boolean;
  /** Gates the primary-env-only "Open in External Editor" context-menu item. */
  isPrimaryEnv?: boolean;
}

interface SourceControlChangesListProps {
  sections: readonly SourceControlListSection[];
  /**
   * Groups rows under a folder header per directory (the default). `false`
   * renders the flat list: every row shows its own directory.
   */
  groupByFolder?: boolean;
}

interface RowMenuState {
  file: WorkingTreeFile;
  sectionIndex: number;
  x: number;
  y: number;
}

// Every row has a fixed pixel height, so the list can place rows it has not
// mounted; rows fill it. Each height includes the 2px gap below the row.
export const SOURCE_CONTROL_ROW_HEIGHT = {
  section: 34,
  folder: 26,
  file: 34,
} as const satisfies Record<SourceControlListRow["kind"], number>;

const NO_COLLAPSED: ReadonlySet<string> = new Set<string>();
const rowKey = (row: SourceControlListRow) => row.key;
const rowHeight = (row: SourceControlListRow) => SOURCE_CONTROL_ROW_HEIGHT[row.kind];

function toggled(current: ReadonlySet<string>, key: string): ReadonlySet<string> {
  const next = new Set(current);
  if (!next.delete(key)) next.add(key);
  return next;
}

/**
 * The Source Control change sections as one virtualized list: only the rows in
 * view mount, so thousands of changes cost the same as a screenful (#119).
 */
export function SourceControlChangesList(props: SourceControlChangesListProps) {
  const { sections } = props;
  const [menu, setMenu] = useState<RowMenuState | null>(null);
  const [collapsedSections, setCollapsedSections] = useState<ReadonlySet<string>>(NO_COLLAPSED);
  const [collapsedFolders, setCollapsedFolders] = useState<ReadonlySet<string>>(NO_COLLAPSED);
  const rows = buildSourceControlRows(sections, {
    groupByFolder: props.groupByFolder ?? true,
    collapsedSections,
    collapsedFolders,
  });

  if (rows.length === 0) {
    return <p className="px-2 py-6 text-center text-xs text-muted-foreground">No changes</p>;
  }

  const menuSection = menu ? sections[menu.sectionIndex] : undefined;

  function renderRow(item: SourceControlListRow) {
    const section = sections[item.sectionIndex];
    if (!section) return null;
    switch (item.kind) {
      case "section":
        return (
          <div className="h-full pb-[2px]">
            <SourceControlSectionHeader
              section={section}
              collapsed={item.collapsed}
              onToggleCollapsed={() =>
                setCollapsedSections((current) => toggled(current, section.title))
              }
            />
          </div>
        );
      case "folder":
        return (
          <div className="h-full px-1 pb-[2px]">
            <SourceControlFolderHeader
              section={section}
              group={item.group}
              collapsed={item.collapsed}
              onToggleCollapsed={() =>
                setCollapsedFolders((current) =>
                  toggled(current, sectionItemKey(section.title, item.group.label)),
                )
              }
            />
          </div>
        );
      case "file":
        return (
          <div className="h-full px-1 pb-[2px]">
            <SourceControlFileRow
              section={section}
              file={item.file}
              showDirectory={item.showDirectory}
              onContextMenu={(x, y) =>
                setMenu({ file: item.file, sectionIndex: item.sectionIndex, x, y })
              }
            />
          </div>
        );
    }
  }

  return (
    <>
      <WindowedList
        className="size-full min-w-0 overflow-x-hidden overscroll-y-contain"
        items={rows}
        itemHeight={rowHeight}
        itemKey={rowKey}
        renderItem={renderRow}
        textSurface="background"
      />
      {menu && menuSection ? (
        <SourceControlRowContextMenu
          state={menu}
          isPrimaryEnv={menuSection.isPrimaryEnv ?? false}
          onView={(file) =>
            (menuSection.onViewFile ?? menuSection.onOpenFile)(file.path, file.area)
          }
          {...(menuSection.onCopyPath ? { onCopyPath: menuSection.onCopyPath } : {})}
          {...(menuSection.onOpenExternalEditor
            ? { onOpenExternalEditor: menuSection.onOpenExternalEditor }
            : {})}
          {...(!menuSection.selectionMode && menuSection.onIgnoreFileName
            ? { onIgnoreFileName: menuSection.onIgnoreFileName }
            : {})}
          {...(!menuSection.selectionMode && menuSection.onIgnoreParentFolder
            ? { onIgnoreParentFolder: menuSection.onIgnoreParentFolder }
            : {})}
          onClose={() => setMenu(null)}
        />
      ) : null}
    </>
  );
}

function sectionSelects(section: SourceControlListSection): boolean {
  return section.selectionMode === true && section.onSelect !== undefined;
}

function SourceControlFolderHeader({
  section,
  group,
  collapsed,
  onToggleCollapsed,
}: {
  section: SourceControlListSection;
  group: SourceControlFolderGroup;
  collapsed: boolean;
  onToggleCollapsed: () => void;
}) {
  const selects = sectionSelects(section);
  const isChecked = selects
    ? (file: WorkingTreeFile) => section.selected?.(file) ?? false
    : section.checked;
  const state = isChecked ? folderCheckboxState(group.files, isChecked) : null;
  const target = folderActionTarget(group);

  // The panel always wires the batch handlers; the per-file loops keep a folder
  // header working for a host that only passes the single-file props.
  function setFolderStaged(files: readonly WorkingTreeFile[], staged: boolean): void {
    if (files.length === 0) return;
    const paths = files.map((file) => file.path);
    const stageFiles = staged ? section.onStageFiles : section.onUnstageFiles;
    if (stageFiles) {
      stageFiles(paths);
      return;
    }
    for (const path of paths) section.onToggle(path);
  }

  function setFolderSelected(files: readonly WorkingTreeFile[], selected: boolean): void {
    if (section.onSelectFiles) {
      section.onSelectFiles(files, selected);
      return;
    }
    for (const file of files) section.onSelect?.(file.path, selected);
  }

  return (
    <div className="flex h-full items-center gap-2 rounded-md px-2">
      {state === null ? null : (
        <Checkbox
          checked={state === "all"}
          indeterminate={state === "mixed"}
          disabled={section.disabled}
          aria-label={
            selects
              ? `${state === "all" ? "Deselect" : "Select"} all files in ${target}`
              : `${state === "all" ? "Unstage" : "Stage"} all files in ${target}`
          }
          onCheckedChange={() => {
            if (selects) {
              setFolderSelected(group.files, state !== "all");
              return;
            }
            if (state === "all") {
              setFolderStaged(group.files, false);
              return;
            }
            // Already-staged files stay staged: only send what is missing.
            setFolderStaged(
              group.files.filter((file) => !(section.checked?.(file) ?? false)),
              true,
            );
          }}
        />
      )}
      <button
        type="button"
        aria-expanded={!collapsed}
        // The label is start-truncated and the count is a bare numeral, so the
        // button spells its own accessible name out in full.
        aria-label={`${group.directory ?? "Repository root"}, ${fileCountLabel(group.files.length)}`}
        onClick={onToggleCollapsed}
        title={group.directory ?? "Repository root"}
        className="flex min-w-0 flex-1 items-center gap-1.5 text-left"
      >
        <ChevronDownIcon
          className={cn(
            "size-3.5 shrink-0 text-muted-foreground transition-transform",
            collapsed && "-rotate-90",
          )}
        />
        {/* Truncates from the start so the deepest folder stays readable. The
            inner bdi isolates the path from the rtl container without putting
            bidi marks into the text the user can copy. */}
        <span
          dir="rtl"
          aria-hidden="true"
          className="min-w-0 truncate text-left font-mono text-xs text-muted-foreground"
        >
          <bdi dir="ltr">{group.label}</bdi>
        </span>
        {/* Count sits next to the label, like the section headers above. */}
        <span aria-hidden="true" className="ml-1 shrink-0 text-xs text-muted-foreground">
          {group.files.length}
        </span>
      </button>
    </div>
  );
}

function rowActionHandler(
  section: SourceControlListSection,
  action: RowActionDescriptor,
  file: WorkingTreeFile,
): (() => void) | undefined {
  switch (action.kind) {
    case "stage": {
      const handler = section.onStageFile;
      return handler ? () => handler(file.path) : undefined;
    }
    case "unstage": {
      const handler = section.onUnstageFile;
      return handler ? () => handler(file.path) : undefined;
    }
    case "discard":
    case "delete": {
      const handler = section.onRequestDiscardFile;
      return handler ? () => handler(file) : undefined;
    }
  }
}

function SourceControlFileRow({
  section,
  file,
  showDirectory,
  onContextMenu,
}: {
  section: SourceControlListSection;
  file: WorkingTreeFile;
  showDirectory: boolean;
  onContextMenu: (x: number, y: number) => void;
}) {
  const { dir, name } = splitFilePath(file.path);
  const isChecked = section.checked?.(file) ?? false;
  const isSelected = section.selected?.(file) ?? false;
  const badge = workingTreeStatusBadge(file.status);
  const rowActions = section.selectionMode
    ? []
    : getRowActions(rowAreaOf(file.area), file.status).flatMap((action) => {
        const onClick = rowActionHandler(section, action, file);
        return onClick ? [{ action, onClick }] : [];
      });
  return (
    <div
      className={cn(
        // No position or transform here: each mounted row stays in its list
        // item's layer instead of adding compositing work of its own (#119).
        "group flex h-full items-center gap-2 rounded-md px-2 hover:bg-accent/50",
        // Indents a grouped row so its name lines up under the folder label.
        !showDirectory && "pl-7",
        section.selectionMode && isSelected && "bg-accent/70",
      )}
      onContextMenu={(event) => {
        event.preventDefault();
        onContextMenu(event.clientX, event.clientY);
      }}
    >
      {section.selectionMode && section.onSelect ? (
        <Checkbox
          checked={isSelected}
          onCheckedChange={(selected) => section.onSelect?.(file.path, selected === true)}
          disabled={section.disabled}
          aria-label={isSelected ? `Deselect ${file.path}` : `Select ${file.path}`}
        />
      ) : section.checked ? (
        <Checkbox
          checked={isChecked}
          onCheckedChange={() => section.onToggle(file.path)}
          disabled={section.disabled}
          aria-label={isChecked ? `Unstage ${file.path}` : `Stage ${file.path}`}
        />
      ) : null}
      <button
        type="button"
        className="flex min-w-0 flex-1 items-center gap-2 text-left"
        onClick={() => section.onOpenFile(file.path, file.area)}
        title={file.path}
      >
        <span
          className={cn(
            "min-w-0 truncate font-mono text-xs",
            // A grouped row owns the whole width; a flat row keeps its natural
            // width and only gives way once the directory hits its floor.
            !showDirectory && "flex-1",
          )}
        >
          {name}
        </span>
        {showDirectory && dir ? (
          // Takes the leftover width, never less than a readable tail.
          <span
            dir="rtl"
            className="min-w-[4.5rem] shrink basis-0 grow truncate text-left text-xs text-muted-foreground"
          >
            <bdi dir="ltr">{dir}</bdi>
          </span>
        ) : null}
        <DiffStatLabel
          additions={file.insertions}
          deletions={file.deletions}
          layout="inline"
          className="ml-auto shrink-0 text-xs"
        />
        <span
          className={cn("w-4 shrink-0 text-center text-xs font-bold", badge.className)}
          title={badge.label}
        >
          {badge.letter}
        </span>
      </button>
      {rowActions.length > 0 ? (
        <div className="pointer-events-none flex shrink-0 items-center gap-0.5 rounded-md bg-accent px-0.5 opacity-0 shadow-sm transition-opacity focus-within:pointer-events-auto focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100">
          {rowActions.map(({ action, onClick }) => (
            <Button
              key={action.kind}
              type="button"
              variant="ghost"
              size="icon-xs"
              disabled={section.disabled}
              aria-label={`${action.label} ${file.path}`}
              title={action.label}
              className={cn(action.destructive && "text-destructive hover:text-destructive")}
              onClick={(event) => {
                event.stopPropagation();
                onClick();
              }}
            >
              <action.icon className="size-3.5" />
            </Button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

interface RowContextMenuProps {
  state: RowMenuState | null;
  isPrimaryEnv: boolean;
  onView: (file: WorkingTreeFile) => void;
  onCopyPath?: (path: string, relative: boolean) => void;
  onOpenExternalEditor?: (path: string) => void;
  onIgnoreFileName?: (path: string) => void;
  onIgnoreParentFolder?: (path: string) => void;
  onClose: () => void;
}

/**
 * Navigation-only right-click menu, anchored at the cursor via a virtual point
 * (see FileBrowserPanel's background menu). Items whose host handler is unwired
 * are dropped so the menu never shows a dead entry.
 */
function SourceControlRowContextMenu(props: RowContextMenuProps) {
  const { state } = props;
  if (!state) return null;
  const { file } = state;

  const handlerFor = (id: RowContextMenuItemId): (() => void) | undefined => {
    switch (id) {
      case "view":
        return () => props.onView(file);
      case "copy-path": {
        const handler = props.onCopyPath;
        return handler ? () => handler(file.path, false) : undefined;
      }
      case "copy-relative-path": {
        const handler = props.onCopyPath;
        return handler ? () => handler(file.path, true) : undefined;
      }
      case "ignore-file-name": {
        const handler = props.onIgnoreFileName;
        return handler ? () => handler(file.path) : undefined;
      }
      case "ignore-parent-folder": {
        const handler = props.onIgnoreParentFolder;
        return handler ? () => handler(file.path) : undefined;
      }
      case "open-external-editor": {
        const handler = props.onOpenExternalEditor;
        return handler ? () => handler(file.path) : undefined;
      }
    }
  };

  const groups = buildRowContextMenu({ isPrimaryEnv: props.isPrimaryEnv })
    .groups.map((group) =>
      group.flatMap((item) => {
        const onClick = handlerFor(item.id);
        return onClick ? [{ item, onClick }] : [];
      }),
    )
    .filter((group) => group.length > 0);
  if (groups.length === 0) return null;

  const anchor = { getBoundingClientRect: () => new DOMRect(state.x, state.y, 0, 0) };

  return (
    <Menu
      open
      onOpenChange={(open) => {
        if (!open) props.onClose();
      }}
    >
      <MenuPopup
        anchor={anchor}
        align="start"
        side="bottom"
        sideOffset={2}
        onClick={(event) => event.stopPropagation()}
        onContextMenu={(event) => event.preventDefault()}
      >
        {groups.map((group, index) => (
          <Fragment key={group.map((entry) => entry.item.id).join(",")}>
            {index > 0 ? <MenuSeparator /> : null}
            {group.map(({ item, onClick }) => (
              <MenuItem
                key={item.id}
                disabled={!item.enabled}
                onClick={() => {
                  onClick();
                  props.onClose();
                }}
              >
                {item.label}
              </MenuItem>
            ))}
          </Fragment>
        ))}
      </MenuPopup>
    </Menu>
  );
}
