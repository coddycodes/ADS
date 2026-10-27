import {
  type EnvironmentId,
  type EditorId,
  type ProjectScript,
  type ResolvedKeybindingsConfig,
  type ServerProvider,
  type ServerSettings,
  type ThreadId,
} from "@coddy/contracts";
import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { MoreHorizontal } from "lucide-react";
import { memo, useMemo } from "react";
import GitActionsControl from "../GitActionsControl";
import { type DraftId } from "~/composerDraftStore";
import { type ProviderInstanceEntry } from "~/providerInstances";
import { type CenterPaneHeaderDensity } from "../centerPaneHeaderDensity";
import { CenterHeaderIconButton } from "../CenterHeaderIconButton";
import {
  type NewProjectScriptInput,
  type ProjectScriptActionResult,
  ProjectScriptsDialogs,
  ProjectScriptsExpandedActions,
  ProjectScriptsMenuItems,
  useProjectScriptsController,
} from "../ProjectScriptsControl";
import { Menu, MenuPopup, MenuSeparator, MenuTrigger } from "../ui/menu";
import { ChatHeaderPanelMenu } from "./ChatHeaderPanelMenu";
import { OpenInExpandedActions, OpenInMenuItems, useOpenInEditorController } from "./OpenInPicker";
import { type ProviderTerminalAction } from "./providerTerminalActions";
import { usePrimaryEnvironmentId } from "../../state/environments";
import { cn } from "~/lib/utils";

interface ChatHeaderActionsProps {
  readonly density: CenterPaneHeaderDensity;
  activeThreadEnvironmentId: EnvironmentId;
  activeThreadId: ThreadId;
  draftId?: DraftId;
  activeProjectName: string | undefined;
  openInCwd: string | null;
  activeProjectScripts: ReadonlyArray<ProjectScript> | undefined;
  preferredScriptId: string | null;
  keybindings: ResolvedKeybindingsConfig;
  availableEditors: ReadonlyArray<EditorId>;
  reserveTitlebarControls: boolean;
  gitCwd: string | null;
  providerStatuses: ReadonlyArray<ServerProvider>;
  settings: Pick<ServerSettings, "providerInstances" | "providers" | "providerSessionDefaults">;
  canCreatePanel: boolean;
  onCreateChatPanel: (entry: ProviderInstanceEntry) => void;
  onReopenChatPanel: (threadId: ThreadId, providerLabel: string) => void;
  onOpenTerminalPanel: () => void;
  onOpenProviderTerminalPanel: (action: ProviderTerminalAction) => void;
  onRunProjectScript: (script: ProjectScript) => void;
  onAddProjectScript: (input: NewProjectScriptInput) => Promise<ProjectScriptActionResult>;
  onUpdateProjectScript: (
    scriptId: string,
    input: NewProjectScriptInput,
  ) => Promise<ProjectScriptActionResult>;
  onDeleteProjectScript: (scriptId: string) => Promise<ProjectScriptActionResult>;
  workspaceUnavailable?: string | null;
}

export function shouldShowOpenInPicker(input: {
  readonly activeProjectName: string | undefined;
  readonly activeThreadEnvironmentId: EnvironmentId;
  readonly primaryEnvironmentId: EnvironmentId | null;
}): boolean {
  return (
    Boolean(input.activeProjectName) &&
    input.primaryEnvironmentId !== null &&
    input.activeThreadEnvironmentId === input.primaryEnvironmentId
  );
}

export const ChatHeaderActions = memo(function ChatHeaderActions({
  density,
  activeThreadEnvironmentId,
  activeThreadId,
  draftId,
  activeProjectName,
  openInCwd,
  activeProjectScripts,
  preferredScriptId,
  keybindings,
  availableEditors,
  reserveTitlebarControls,
  gitCwd,
  providerStatuses,
  settings,
  canCreatePanel,
  onCreateChatPanel,
  onReopenChatPanel,
  onOpenTerminalPanel,
  onOpenProviderTerminalPanel,
  onRunProjectScript,
  onAddProjectScript,
  onUpdateProjectScript,
  onDeleteProjectScript,
  workspaceUnavailable = null,
}: ChatHeaderActionsProps) {
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const showOpenInPicker = shouldShowOpenInPicker({
    activeProjectName,
    activeThreadEnvironmentId,
    primaryEnvironmentId,
  });
  const projectScripts = useProjectScriptsController({
    scripts: activeProjectScripts ?? [],
    keybindings,
    preferredScriptId,
    enabled: activeProjectScripts !== undefined && workspaceUnavailable === null,
    disabledReason: workspaceUnavailable,
    onRunScript: onRunProjectScript,
    onAddScript: onAddProjectScript,
    onUpdateScript: onUpdateProjectScript,
    onDeleteScript: onDeleteProjectScript,
  });
  const openInEditor = useOpenInEditorController({
    environmentId: activeThreadEnvironmentId,
    keybindings,
    availableEditors,
    openInCwd,
    enableShortcut: showOpenInPicker && workspaceUnavailable === null,
  });
  const projectScriptActionsAvailable = activeProjectScripts !== undefined;
  const openInActionsAvailable =
    showOpenInPicker && workspaceUnavailable === null && openInEditor.options.length > 0;
  const compactActionsAvailable = projectScriptActionsAvailable || openInActionsAvailable;
  const activeThreadRef = useMemo(
    () => scopeThreadRef(activeThreadEnvironmentId, activeThreadId),
    [activeThreadEnvironmentId, activeThreadId],
  );

  return (
    <div
      data-chat-header-actions
      className={cn(
        "relative z-10 flex shrink-0 items-center justify-end bg-background [-webkit-app-region:no-drag]",
        density === "compact" ? "gap-1" : "gap-2 @3xl/header-actions:gap-3",
        reserveTitlebarControls ? "pr-[4.5rem]" : "pr-2",
      )}
    >
      <ChatHeaderPanelMenu
        providerStatuses={providerStatuses}
        settings={settings}
        canCreatePanel={canCreatePanel && workspaceUnavailable === null}
        unavailableReason={workspaceUnavailable}
        environmentId={activeThreadEnvironmentId}
        hostThreadId={activeThreadId}
        onCreateChatPanel={onCreateChatPanel}
        onReopenChatPanel={onReopenChatPanel}
        onOpenTerminalPanel={onOpenTerminalPanel}
        onOpenProviderTerminalPanel={onOpenProviderTerminalPanel}
        onAddCustomAction={projectScripts.openAddDialog}
      />
      {density === "expanded" ? (
        <>
          <ProjectScriptsExpandedActions controller={projectScripts} />
          {showOpenInPicker && workspaceUnavailable === null ? (
            <OpenInExpandedActions controller={openInEditor} />
          ) : null}
        </>
      ) : compactActionsAvailable ? (
        <Menu>
          <MenuTrigger render={<CenterHeaderIconButton aria-label="More workspace actions" />}>
            <MoreHorizontal className="size-4" />
          </MenuTrigger>
          <MenuPopup align="end" className="min-w-56">
            {projectScriptActionsAvailable ? (
              <ProjectScriptsMenuItems controller={projectScripts} />
            ) : null}
            {projectScriptActionsAvailable && openInActionsAvailable ? <MenuSeparator /> : null}
            {openInActionsAvailable ? <OpenInMenuItems controller={openInEditor} /> : null}
          </MenuPopup>
        </Menu>
      ) : null}
      <ProjectScriptsDialogs controller={projectScripts} />
      {activeProjectName && (
        <GitActionsControl
          gitCwd={gitCwd}
          activeThreadRef={activeThreadRef}
          {...(draftId ? { draftId } : {})}
          // Trigger hidden: git actions live in the Source Control panel, but
          // the control must stay mounted for its thread-branch sync effect.
          hideTrigger
          workspaceUnavailable={workspaceUnavailable}
        />
      )}
    </div>
  );
});
