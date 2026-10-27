import type {
  ProjectScript,
  ProjectScriptIcon,
  ResolvedKeybindingsConfig,
} from "@coddy/contracts";
import {
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@coddy/client-runtime/state/runtime";
import {
  BugIcon,
  ChevronDownIcon,
  FlaskConicalIcon,
  HammerIcon,
  ListChecksIcon,
  PlayIcon,
  PlusIcon,
  SettingsIcon,
  WrenchIcon,
} from "lucide-react";
import React, {
  type Dispatch,
  type FormEvent,
  type KeyboardEvent,
  type ReactNode,
  type SetStateAction,
  useCallback,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  keybindingValueForCommand,
  decodeProjectScriptKeybindingRule,
} from "~/lib/projectScriptKeybindings";
import { keybindingFromKeyboardEvent } from "~/components/settings/KeybindingsSettings.logic";
import {
  commandForProjectScript,
  nextProjectScriptId,
  primaryProjectScript,
} from "~/projectScripts";
import { shortcutLabelForCommand } from "~/keybindings";
import {
  AlertDialog,
  AlertDialogClose,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "./ui/alert-dialog";
import { Button } from "./ui/button";
import {
  Dialog,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
} from "./ui/dialog";
import { Group, GroupSeparator } from "./ui/group";
import { Input } from "./ui/input";
import { Label } from "./ui/label";
import { Menu, MenuItem, MenuPopup, MenuShortcut, MenuTrigger } from "./ui/menu";
import { Popover, PopoverPopup, PopoverTrigger } from "./ui/popover";
import { Switch } from "./ui/switch";
import { Textarea } from "./ui/textarea";
import { Tooltip, TooltipPopup, TooltipTrigger } from "./ui/tooltip";

const SCRIPT_ICONS: Array<{ id: ProjectScriptIcon; label: string }> = [
  { id: "play", label: "Play" },
  { id: "test", label: "Test" },
  { id: "lint", label: "Lint" },
  { id: "configure", label: "Configure" },
  { id: "build", label: "Build" },
  { id: "debug", label: "Debug" },
];

function ScriptIcon({
  icon,
  className = "size-3.5",
}: {
  icon: ProjectScriptIcon;
  className?: string;
}) {
  if (icon === "test") return <FlaskConicalIcon className={className} />;
  if (icon === "lint") return <ListChecksIcon className={className} />;
  if (icon === "configure") return <WrenchIcon className={className} />;
  if (icon === "build") return <HammerIcon className={className} />;
  if (icon === "debug") return <BugIcon className={className} />;
  return <PlayIcon className={className} />;
}

export interface NewProjectScriptInput {
  name: string;
  command: string;
  icon: ProjectScriptIcon;
  runOnWorktreeCreate: boolean;
  keybinding: string | null;
  /** Optional URL to open in the in-app preview when this script runs. */
  previewUrl: string | null;
  /** When true, automatically open the preview panel pointed at `previewUrl`. */
  autoOpenPreview: boolean;
}

export type ProjectScriptActionResult = AtomCommandResult<void, unknown>;

export interface ProjectScriptsControlProps {
  scripts: ReadonlyArray<ProjectScript>;
  keybindings: ResolvedKeybindingsConfig;
  preferredScriptId?: string | null;
  enabled?: boolean;
  disabledReason?: string | null;
  onRunScript: (script: ProjectScript) => void;
  onAddScript: (input: NewProjectScriptInput) => Promise<ProjectScriptActionResult>;
  onUpdateScript: (
    scriptId: string,
    input: NewProjectScriptInput,
  ) => Promise<ProjectScriptActionResult>;
  onDeleteScript: (scriptId: string) => Promise<ProjectScriptActionResult>;
}

export interface ProjectScriptsController {
  readonly scripts: ReadonlyArray<ProjectScript>;
  readonly primaryScript: ProjectScript | null;
  readonly disabledReason: string | null;
  readonly openAddDialog: () => void;
  readonly openEditDialog: (script: ProjectScript) => void;
  readonly runScript: (script: ProjectScript) => void;
}

interface ProjectScriptsControllerState extends ProjectScriptsController {
  readonly keybindings: ResolvedKeybindingsConfig;
  readonly addScriptFormId: string;
  readonly editingScriptId: string | null;
  readonly dialogOpen: boolean;
  readonly name: string;
  readonly command: string;
  readonly icon: ProjectScriptIcon;
  readonly iconPickerOpen: boolean;
  readonly runOnWorktreeCreate: boolean;
  readonly keybinding: string;
  readonly previewUrl: string;
  readonly autoOpenPreview: boolean;
  readonly validationError: string | null;
  readonly deleteConfirmOpen: boolean;
  readonly setEditingScriptId: Dispatch<SetStateAction<string | null>>;
  readonly setDialogOpen: Dispatch<SetStateAction<boolean>>;
  readonly setName: Dispatch<SetStateAction<string>>;
  readonly setCommand: Dispatch<SetStateAction<string>>;
  readonly setIcon: Dispatch<SetStateAction<ProjectScriptIcon>>;
  readonly setIconPickerOpen: Dispatch<SetStateAction<boolean>>;
  readonly setRunOnWorktreeCreate: Dispatch<SetStateAction<boolean>>;
  readonly setKeybinding: Dispatch<SetStateAction<string>>;
  readonly setPreviewUrl: Dispatch<SetStateAction<string>>;
  readonly setAutoOpenPreview: Dispatch<SetStateAction<boolean>>;
  readonly setValidationError: Dispatch<SetStateAction<string | null>>;
  readonly setDeleteConfirmOpen: Dispatch<SetStateAction<boolean>>;
  readonly captureKeybinding: (event: KeyboardEvent<HTMLInputElement>) => void;
  readonly submitScript: (event: FormEvent) => Promise<void>;
  readonly confirmDeleteScript: () => void;
}

function projectActionsUnavailableReason(
  enabled: boolean,
  disabledReason: string | null,
): string | null {
  return enabled ? null : (disabledReason ?? "Project actions are unavailable.");
}

export function useProjectScriptsController({
  scripts,
  keybindings,
  preferredScriptId = null,
  enabled = true,
  disabledReason = null,
  onRunScript,
  onAddScript,
  onUpdateScript,
  onDeleteScript,
}: ProjectScriptsControlProps): ProjectScriptsController {
  const addScriptFormId = React.useId();
  const [editingScriptId, setEditingScriptId] = useState<string | null>(null);
  const [dialogOpen, setDialogOpen] = useState(false);
  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [icon, setIcon] = useState<ProjectScriptIcon>("play");
  const [iconPickerOpen, setIconPickerOpen] = useState(false);
  const [runOnWorktreeCreate, setRunOnWorktreeCreate] = useState(false);
  const [keybinding, setKeybinding] = useState("");
  const [previewUrl, setPreviewUrl] = useState("");
  const [autoOpenPreview, setAutoOpenPreview] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(() =>
    projectActionsUnavailableReason(enabled, disabledReason),
  );
  const [deleteConfirmOpen, setDeleteConfirmOpen] = useState(false);
  const availabilityRef = useRef({ enabled, disabledReason });
  useLayoutEffect(() => {
    availabilityRef.current = { enabled, disabledReason };
  }, [disabledReason, enabled]);
  const currentDisabledReason = useCallback(() => {
    const availability = availabilityRef.current;
    return projectActionsUnavailableReason(availability.enabled, availability.disabledReason);
  }, []);

  const [shownAvailability, setShownAvailability] = useState({ enabled, disabledReason });
  if (
    shownAvailability.enabled !== enabled ||
    shownAvailability.disabledReason !== disabledReason
  ) {
    setShownAvailability({ enabled, disabledReason });
    const reason = projectActionsUnavailableReason(enabled, disabledReason);
    if (reason !== null) {
      setDialogOpen(false);
      setIconPickerOpen(false);
      setDeleteConfirmOpen(false);
      setValidationError(reason);
    }
  }

  const primaryScript = useMemo(() => {
    if (preferredScriptId) {
      const preferred = scripts.find((script) => script.id === preferredScriptId);
      if (preferred) return preferred;
    }
    return primaryProjectScript(scripts);
  }, [preferredScriptId, scripts]);
  const captureKeybinding = useCallback((event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Tab") return;
    event.preventDefault();
    if (event.key === "Backspace" || event.key === "Delete") {
      setKeybinding("");
      return;
    }
    const next = keybindingFromKeyboardEvent(event, navigator.platform);
    if (!next) return;
    setKeybinding(next);
  }, []);

  const submitScript = useCallback(
    async (event: FormEvent) => {
      event.preventDefault();
      const unavailableReason = currentDisabledReason();
      if (unavailableReason !== null) {
        setValidationError(unavailableReason);
        return;
      }
      const trimmedName = name.trim();
      const trimmedCommand = command.trim();
      if (trimmedName.length === 0) {
        setValidationError("Name is required.");
        return;
      }
      if (trimmedCommand.length === 0) {
        setValidationError("Command is required.");
        return;
      }

      setValidationError(null);
      let payload: NewProjectScriptInput;
      try {
        const scriptIdForValidation =
          editingScriptId ??
          nextProjectScriptId(
            trimmedName,
            scripts.map((script) => script.id),
          );
        const keybindingRule = decodeProjectScriptKeybindingRule({
          keybinding,
          command: commandForProjectScript(scriptIdForValidation),
        });
        const trimmedPreviewUrl = previewUrl.trim();
        payload = {
          name: trimmedName,
          command: trimmedCommand,
          icon,
          runOnWorktreeCreate,
          keybinding: keybindingRule?.key ?? null,
          previewUrl: trimmedPreviewUrl.length > 0 ? trimmedPreviewUrl : null,
          autoOpenPreview: trimmedPreviewUrl.length > 0 ? autoOpenPreview : false,
        } satisfies NewProjectScriptInput;
      } catch (error) {
        setValidationError(error instanceof Error ? error.message : "Failed to save action.");
        return;
      }

      const result = editingScriptId
        ? await onUpdateScript(editingScriptId, payload)
        : await onAddScript(payload);
      const lateUnavailableReason = currentDisabledReason();
      if (lateUnavailableReason !== null) {
        setValidationError(lateUnavailableReason);
        return;
      }
      if (result._tag === "Failure") {
        if (!isAtomCommandInterrupted(result)) {
          const error = squashAtomCommandFailure(result);
          setValidationError(error instanceof Error ? error.message : "Failed to save action.");
        }
        return;
      }
      setDialogOpen(false);
      setIconPickerOpen(false);
    },
    [
      autoOpenPreview,
      command,
      currentDisabledReason,
      editingScriptId,
      icon,
      keybinding,
      name,
      onAddScript,
      onUpdateScript,
      previewUrl,
      runOnWorktreeCreate,
      scripts,
    ],
  );

  const openAddDialog = useCallback(() => {
    if (currentDisabledReason() !== null) return;
    setEditingScriptId(null);
    setName("");
    setCommand("");
    setIcon("play");
    setIconPickerOpen(false);
    setRunOnWorktreeCreate(false);
    setKeybinding("");
    setPreviewUrl("");
    setAutoOpenPreview(false);
    setValidationError(null);
    setDialogOpen(true);
  }, [currentDisabledReason]);

  const openEditDialog = useCallback(
    (script: ProjectScript) => {
      if (currentDisabledReason() !== null) return;
      setEditingScriptId(script.id);
      setName(script.name);
      setCommand(script.command);
      setIcon(script.icon);
      setIconPickerOpen(false);
      setRunOnWorktreeCreate(script.runOnWorktreeCreate);
      setKeybinding(
        keybindingValueForCommand(keybindings, commandForProjectScript(script.id)) ?? "",
      );
      setPreviewUrl(script.previewUrl ?? "");
      setAutoOpenPreview(script.autoOpenPreview ?? false);
      setValidationError(null);
      setDialogOpen(true);
    },
    [currentDisabledReason, keybindings],
  );

  const runScript = useCallback(
    (script: ProjectScript) => {
      if (currentDisabledReason() === null) onRunScript(script);
    },
    [currentDisabledReason, onRunScript],
  );

  const confirmDeleteScript = useCallback(() => {
    if (!editingScriptId) return;
    setDeleteConfirmOpen(false);
    setDialogOpen(false);
    const unavailableReason = currentDisabledReason();
    if (unavailableReason !== null) {
      setValidationError(unavailableReason);
      return;
    }
    void onDeleteScript(editingScriptId);
  }, [currentDisabledReason, editingScriptId, onDeleteScript]);

  const controller: ProjectScriptsControllerState = {
    scripts,
    primaryScript,
    disabledReason,
    openAddDialog,
    openEditDialog,
    runScript,
    keybindings,
    addScriptFormId,
    editingScriptId,
    dialogOpen,
    name,
    command,
    icon,
    iconPickerOpen,
    runOnWorktreeCreate,
    keybinding,
    previewUrl,
    autoOpenPreview,
    validationError,
    deleteConfirmOpen,
    setEditingScriptId,
    setDialogOpen,
    setName,
    setCommand,
    setIcon,
    setIconPickerOpen,
    setRunOnWorktreeCreate,
    setKeybinding,
    setPreviewUrl,
    setAutoOpenPreview,
    setValidationError,
    setDeleteConfirmOpen,
    captureKeybinding,
    submitScript,
    confirmDeleteScript,
  };
  return controller;
}

const dropdownItemClassName =
  "data-highlighted:bg-transparent data-highlighted:text-foreground hover:bg-accent hover:text-accent-foreground focus-visible:bg-accent focus-visible:text-accent-foreground data-highlighted:hover:bg-accent data-highlighted:hover:text-accent-foreground data-highlighted:focus-visible:bg-accent data-highlighted:focus-visible:text-accent-foreground";

export function ProjectScriptsMenuItems({
  controller,
}: {
  controller: ProjectScriptsController;
}): ReactNode {
  const state = controller as ProjectScriptsControllerState;
  return (
    <>
      {controller.scripts.map((script) => {
        const shortcutLabel = shortcutLabelForCommand(
          state.keybindings,
          commandForProjectScript(script.id),
        );
        return (
          <MenuItem
            key={script.id}
            className={`group ${dropdownItemClassName}`}
            disabled={controller.disabledReason !== null}
            title={controller.disabledReason ?? undefined}
            onClick={() => controller.runScript(script)}
          >
            <ScriptIcon icon={script.icon} className="size-4" />
            <span className="truncate">
              {script.runOnWorktreeCreate ? `${script.name} (setup)` : script.name}
            </span>
            <span className="relative ms-auto flex h-6 min-w-6 items-center justify-end">
              {shortcutLabel && (
                <MenuShortcut className="ms-0 transition-opacity group-hover:opacity-0 group-focus-visible:opacity-0">
                  {shortcutLabel}
                </MenuShortcut>
              )}
              <Button
                type="button"
                variant="ghost"
                size="icon-xs"
                className="absolute right-0 top-1/2 size-6 -translate-y-1/2 opacity-0 pointer-events-none transition-opacity group-hover:opacity-100 group-hover:pointer-events-auto group-focus-visible:opacity-100 group-focus-visible:pointer-events-auto"
                aria-label={`Edit ${script.name}`}
                disabled={controller.disabledReason !== null}
                title={controller.disabledReason ?? undefined}
                onPointerDown={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                }}
                onClick={(event) => {
                  event.preventDefault();
                  event.stopPropagation();
                  controller.openEditDialog(script);
                }}
              >
                <SettingsIcon className="size-3.5" />
              </Button>
            </span>
          </MenuItem>
        );
      })}
      <MenuItem
        className={dropdownItemClassName}
        disabled={controller.disabledReason !== null}
        title={controller.disabledReason ?? undefined}
        onClick={controller.openAddDialog}
      >
        <PlusIcon className="size-4" />
        Add action
      </MenuItem>
    </>
  );
}

export function ProjectScriptsExpandedActions({
  controller,
}: {
  controller: ProjectScriptsController;
}): ReactNode {
  const { primaryScript } = controller;
  if (!primaryScript) return null;

  return (
    <Group aria-label="Project scripts">
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              size="xs"
              variant="outline"
              aria-label={`Run ${primaryScript.name}`}
              disabled={controller.disabledReason !== null}
              title={controller.disabledReason ?? undefined}
              onClick={() => controller.runScript(primaryScript)}
            />
          }
        >
          <ScriptIcon icon={primaryScript.icon} />
          <span className="sr-only @3xl/header-actions:not-sr-only @3xl/header-actions:ml-0.5">
            {primaryScript.name}
          </span>
        </TooltipTrigger>
        <TooltipPopup side="top">
          {controller.disabledReason ?? `Run ${primaryScript.name}`}
        </TooltipPopup>
      </Tooltip>
      <GroupSeparator className="hidden @3xl/header-actions:block" />
      <Menu highlightItemOnHover={false}>
        <MenuTrigger
          render={
            <Button
              size="icon-xs"
              variant="outline"
              aria-label="Script actions"
              disabled={controller.disabledReason !== null}
              title={controller.disabledReason ?? undefined}
            />
          }
        >
          <ChevronDownIcon className="size-4" />
        </MenuTrigger>
        <MenuPopup align="end">
          <ProjectScriptsMenuItems controller={controller} />
        </MenuPopup>
      </Menu>
    </Group>
  );
}

export function ProjectScriptsDialogs({
  controller,
}: {
  controller: ProjectScriptsController;
}): ReactNode {
  const {
    addScriptFormId,
    editingScriptId,
    dialogOpen,
    name,
    command,
    icon,
    iconPickerOpen,
    runOnWorktreeCreate,
    keybinding,
    previewUrl,
    autoOpenPreview,
    validationError,
    deleteConfirmOpen,
    setEditingScriptId,
    setDialogOpen,
    setName,
    setCommand,
    setIcon,
    setIconPickerOpen,
    setRunOnWorktreeCreate,
    setKeybinding,
    setPreviewUrl,
    setAutoOpenPreview,
    setValidationError,
    setDeleteConfirmOpen,
    captureKeybinding,
    submitScript,
    confirmDeleteScript,
  } = controller as ProjectScriptsControllerState;
  const isEditing = editingScriptId !== null;

  return (
    <>
      <Dialog
        onOpenChange={(open) => {
          setDialogOpen(open);
          if (!open) {
            setIconPickerOpen(false);
          }
        }}
        onOpenChangeComplete={(open) => {
          if (open) return;
          setEditingScriptId(null);
          setName("");
          setCommand("");
          setIcon("play");
          setRunOnWorktreeCreate(false);
          setKeybinding("");
          setPreviewUrl("");
          setAutoOpenPreview(false);
          setValidationError(null);
        }}
        open={dialogOpen}
      >
        <DialogPopup>
          <DialogHeader>
            <DialogTitle>{isEditing ? "Edit Action" : "Add Action"}</DialogTitle>
            <DialogDescription>
              Actions are project-scoped commands you can run from the top bar or keybindings.
            </DialogDescription>
          </DialogHeader>
          <DialogPanel>
            <form id={addScriptFormId} className="space-y-4" onSubmit={submitScript}>
              <div className="space-y-1.5">
                <Label htmlFor="script-name">Name</Label>
                <div className="flex items-center gap-2">
                  <Popover onOpenChange={setIconPickerOpen} open={iconPickerOpen}>
                    <PopoverTrigger
                      render={
                        <Button
                          type="button"
                          variant="outline"
                          className="size-9 shrink-0 hover:bg-popover active:bg-popover data-pressed:bg-popover data-pressed:shadow-xs/5 data-pressed:before:shadow-[0_1px_--theme(--color-black/4%)] dark:data-pressed:before:shadow-[0_-1px_--theme(--color-white/6%)]"
                          aria-label="Choose icon"
                        />
                      }
                    >
                      <ScriptIcon icon={icon} className="size-4.5" />
                    </PopoverTrigger>
                    <PopoverPopup align="start">
                      <div className="grid grid-cols-3 gap-2">
                        {SCRIPT_ICONS.map((entry) => {
                          const isSelected = entry.id === icon;
                          return (
                            <button
                              key={entry.id}
                              type="button"
                              className={`relative flex flex-col items-center gap-2 rounded-md border px-2 py-2 text-xs ${
                                isSelected
                                  ? "border-primary/70 bg-primary/10"
                                  : "border-border/70 hover:bg-accent/60"
                              }`}
                              onClick={() => {
                                setIcon(entry.id);
                                setIconPickerOpen(false);
                              }}
                            >
                              <ScriptIcon icon={entry.id} className="size-4" />
                              <span>{entry.label}</span>
                            </button>
                          );
                        })}
                      </div>
                    </PopoverPopup>
                  </Popover>
                  <Input
                    id="script-name"
                    autoFocus
                    placeholder="Test"
                    value={name}
                    onChange={(event) => setName(event.target.value)}
                  />
                </div>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="script-keybinding">Keybinding</Label>
                <Input
                  id="script-keybinding"
                  placeholder="Press shortcut"
                  value={keybinding}
                  readOnly
                  onKeyDown={captureKeybinding}
                />
                <p className="text-xs text-muted-foreground">
                  Press a shortcut. Use <code>Backspace</code> to clear.
                </p>
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="script-command">Command</Label>
                <Textarea
                  id="script-command"
                  placeholder="bun test"
                  value={command}
                  onChange={(event) => setCommand(event.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="script-preview-url">Preview URL (optional)</Label>
                <Input
                  id="script-preview-url"
                  placeholder="http://localhost:5173"
                  value={previewUrl}
                  onChange={(event) => setPreviewUrl(event.target.value)}
                />
                <p className="text-xs text-muted-foreground">
                  Open this URL in the in-app preview when this action runs.
                </p>
              </div>
              <label className="flex items-center justify-between gap-3 rounded-md border border-border/70 px-3 py-2 text-sm">
                <span>Run automatically on worktree creation</span>
                <Switch
                  checked={runOnWorktreeCreate}
                  onCheckedChange={(checked) => setRunOnWorktreeCreate(Boolean(checked))}
                />
              </label>
              <label
                className={`flex items-center justify-between gap-3 rounded-md border border-border/70 px-3 py-2 text-sm ${
                  previewUrl.trim().length === 0 ? "opacity-60" : ""
                }`}
              >
                <span>Open preview automatically when this action runs</span>
                <Switch
                  checked={autoOpenPreview}
                  disabled={previewUrl.trim().length === 0}
                  onCheckedChange={(checked) => setAutoOpenPreview(Boolean(checked))}
                />
              </label>
              {validationError && <p className="text-sm text-destructive">{validationError}</p>}
            </form>
          </DialogPanel>
          <DialogFooter>
            {isEditing && (
              <Button
                type="button"
                variant="destructive-outline"
                className="mr-auto"
                onClick={() => setDeleteConfirmOpen(true)}
              >
                Delete
              </Button>
            )}
            <Button
              type="button"
              variant="outline"
              onClick={() => {
                setDialogOpen(false);
              }}
            >
              Cancel
            </Button>
            <Button form={addScriptFormId} type="submit">
              {isEditing ? "Save changes" : "Save action"}
            </Button>
          </DialogFooter>
        </DialogPopup>
      </Dialog>

      <AlertDialog open={deleteConfirmOpen} onOpenChange={setDeleteConfirmOpen}>
        <AlertDialogPopup>
          <AlertDialogHeader>
            <AlertDialogTitle>Delete action "{name}"?</AlertDialogTitle>
            <AlertDialogDescription>This action cannot be undone.</AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogClose render={<Button variant="outline" />}>Cancel</AlertDialogClose>
            <Button variant="destructive" onClick={confirmDeleteScript}>
              Delete action
            </Button>
          </AlertDialogFooter>
        </AlertDialogPopup>
      </AlertDialog>
    </>
  );
}

export default function ProjectScriptsControl(props: ProjectScriptsControlProps) {
  const controller = useProjectScriptsController(props);
  return (
    <>
      <ProjectScriptsExpandedActions controller={controller} />
      <ProjectScriptsDialogs controller={controller} />
    </>
  );
}
