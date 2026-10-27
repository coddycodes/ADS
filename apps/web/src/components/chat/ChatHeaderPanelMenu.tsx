import type { EnvironmentId, ServerProvider, ServerSettings, ThreadId } from "@coddy/contracts";
import { HistoryIcon, PlusIcon, TerminalSquare } from "lucide-react";
import { memo, useMemo, type ReactElement } from "react";

import { panelProviderLabel } from "~/centerPanelAdoption";
import { useArchivedThreadSnapshots } from "~/lib/archivedThreadsState";
import type { ProviderInstanceEntry } from "~/providerInstances";
import { formatRelativeTimeLabel } from "~/timestampFormat";
import { CenterHeaderIconButton } from "../CenterHeaderIconButton";
import {
  Menu,
  MenuItem,
  MenuPopup,
  MenuSeparator,
  MenuSub,
  MenuSubPopup,
  MenuSubTrigger,
  MenuTrigger,
} from "../ui/menu";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";
import { ProviderInstanceIcon } from "./ProviderInstanceIcon";
import { buildProviderAgentActions } from "./providerAgentActions";
import type { ProviderTerminalAction } from "./providerTerminalActions";

interface ChatHeaderPanelMenuProps {
  readonly providerStatuses: ReadonlyArray<ServerProvider>;
  readonly settings: Pick<
    ServerSettings,
    "providerInstances" | "providers" | "providerSessionDefaults"
  >;
  /** False when the host thread can't yet spawn sibling panels (no thread ref). */
  readonly canCreatePanel: boolean;
  readonly environmentId: EnvironmentId;
  readonly hostThreadId: ThreadId;
  readonly onCreateChatPanel: (entry: ProviderInstanceEntry) => void;
  readonly onReopenChatPanel: (threadId: ThreadId, providerLabel: string) => void;
  readonly onOpenTerminalPanel: () => void;
  readonly onOpenProviderTerminalPanel: (action: ProviderTerminalAction) => void;
  readonly onAddCustomAction: () => void;
  readonly unavailableReason?: string | null;
}

const PANEL_UNAVAILABLE_REASON = "Available once this thread has started.";
const MAX_CLOSED_CHATS = 10;

function DisabledReasonTooltip(props: { reason: string; trigger: ReactElement }) {
  return (
    <Tooltip>
      <TooltipTrigger render={props.trigger} />
      <TooltipPopup side="top">{props.reason}</TooltipPopup>
    </Tooltip>
  );
}

/**
 * Lists the host's closed (archived) chat panels, newest first. Rendered inside
 * the menu popup, so it reads archived threads only while the menu is open.
 */
function ReopenClosedChatMenu(props: {
  environmentId: EnvironmentId;
  hostThreadId: ThreadId;
  disabledReason: string | null;
  onReopen: (threadId: ThreadId, providerLabel: string) => void;
}) {
  const environmentIds = useMemo(() => [props.environmentId], [props.environmentId]);
  const { snapshots, isLoading, error, refresh } = useArchivedThreadSnapshots(environmentIds);
  const closedChats = (snapshots[0]?.snapshot.threads ?? [])
    .flatMap((thread) =>
      thread.kind === "panel" &&
      thread.hostThreadId === props.hostThreadId &&
      thread.archivedAt !== null
        ? [{ ...thread, archivedAt: thread.archivedAt }]
        : [],
    )
    .toSorted((left, right) => right.archivedAt.localeCompare(left.archivedAt))
    .slice(0, MAX_CLOSED_CHATS);
  const available = props.disabledReason === null && closedChats.length > 0;
  if (props.disabledReason === null && closedChats.length === 0 && error !== null) {
    // An empty list here would hide closed chats that exist; offer a retry instead.
    return (
      <MenuItem closeOnClick={false} onClick={refresh}>
        <HistoryIcon className="size-4" />
        <span className="flex min-w-0 flex-col">
          <span>Reopen closed chat</span>
          <span className="text-xs text-muted-foreground">
            Couldn't load closed chats. Select to retry.
          </span>
        </span>
      </MenuItem>
    );
  }
  const trigger = (
    <MenuSubTrigger
      className={props.disabledReason ? "data-disabled:pointer-events-auto" : undefined}
      disabled={!available}
    >
      <HistoryIcon className="size-4" />
      <span className="flex min-w-0 flex-col">
        <span>Reopen closed chat</span>
        {props.disabledReason === null && closedChats.length === 0 ? (
          <span className="text-xs text-muted-foreground">
            {isLoading ? "Loading closed chats…" : "No closed chats"}
          </span>
        ) : null}
      </span>
    </MenuSubTrigger>
  );
  return (
    <MenuSub>
      {props.disabledReason ? (
        <DisabledReasonTooltip reason={props.disabledReason} trigger={trigger} />
      ) : (
        trigger
      )}
      {available ? (
        <MenuSubPopup className="min-w-52">
          {closedChats.map((thread) => {
            const label = panelProviderLabel(thread.title) ?? thread.title;
            return (
              <MenuItem key={thread.id} onClick={() => props.onReopen(thread.id, label)}>
                <span className="truncate">{label}</span>
                <span className="ms-auto shrink-0 ps-3 text-xs text-muted-foreground">
                  {formatRelativeTimeLabel(thread.archivedAt)}
                </span>
              </MenuItem>
            );
          })}
        </MenuSubPopup>
      ) : null}
    </MenuSub>
  );
}

/**
 * The chat-header "+" menu: create a new chat panel for any enabled provider
 * instance, reopen a closed one, open a center terminal panel, or add a custom
 * project action (the entry point that replaces ProjectScriptsControl's old
 * bare "+").
 */
export const ChatHeaderPanelMenu = memo(function ChatHeaderPanelMenu({
  providerStatuses,
  settings,
  canCreatePanel,
  environmentId,
  hostThreadId,
  onCreateChatPanel,
  onReopenChatPanel,
  onOpenTerminalPanel,
  onOpenProviderTerminalPanel,
  onAddCustomAction,
  unavailableReason = null,
}: ChatHeaderPanelMenuProps) {
  const agentActions = buildProviderAgentActions(providerStatuses, settings);
  const chatActions = agentActions.filter((action) => action.kind === "chat");
  const terminalActions = agentActions.filter((action) => action.kind === "terminal");

  return (
    <Menu>
      <MenuTrigger render={<CenterHeaderIconButton aria-label="New panel" />}>
        <PlusIcon className="size-4" />
      </MenuTrigger>
      <MenuPopup align="end" className="min-w-52">
        {chatActions.map((action) => {
          const disabled = action.disabled || !canCreatePanel;
          const reason = canCreatePanel
            ? action.disabledReason
            : (unavailableReason ?? PANEL_UNAVAILABLE_REASON);
          const menuItem = (
            <MenuItem
              key={action.value}
              className={disabled ? "data-disabled:pointer-events-auto" : undefined}
              disabled={disabled}
              onClick={() => onCreateChatPanel(action.entry)}
            >
              <ProviderInstanceIcon
                driverKind={action.entry.driverKind}
                displayName={action.entry.displayName}
                accentColor={action.entry.accentColor}
                iconClassName="size-4"
              />
              <span className="truncate">{action.label}</span>
            </MenuItem>
          );
          return disabled && reason ? (
            <DisabledReasonTooltip key={action.value} reason={reason} trigger={menuItem} />
          ) : (
            menuItem
          );
        })}
        <ReopenClosedChatMenu
          environmentId={environmentId}
          hostThreadId={hostThreadId}
          disabledReason={canCreatePanel ? null : (unavailableReason ?? PANEL_UNAVAILABLE_REASON)}
          onReopen={onReopenChatPanel}
        />
        <MenuSeparator />
        {canCreatePanel ? (
          <MenuItem onClick={onOpenTerminalPanel}>
            <TerminalSquare className="size-4" />
            Open Terminal
          </MenuItem>
        ) : (
          <DisabledReasonTooltip
            reason={unavailableReason ?? PANEL_UNAVAILABLE_REASON}
            trigger={
              <MenuItem className="data-disabled:pointer-events-auto" disabled>
                <TerminalSquare className="size-4" />
                Open Terminal
              </MenuItem>
            }
          />
        )}
        {terminalActions.length > 0 ? (
          <>
            <MenuSeparator />
            {terminalActions.map((action) => {
              const terminalAction = action.terminalAction;
              const disabled = action.disabled || !canCreatePanel;
              const reason = !canCreatePanel
                ? (unavailableReason ?? PANEL_UNAVAILABLE_REASON)
                : action.disabledReason;
              const menuItem = (
                <MenuItem
                  key={action.value}
                  className={disabled ? "data-disabled:pointer-events-auto" : undefined}
                  disabled={disabled}
                  onClick={() => {
                    if (disabled) {
                      return;
                    }
                    if (terminalAction.command !== null) {
                      if (terminalAction.fallback) {
                        console.warn("Provider session default fallback", terminalAction.fallback);
                      }
                      onOpenProviderTerminalPanel(terminalAction);
                    }
                  }}
                >
                  <ProviderInstanceIcon
                    driverKind={action.entry.driverKind}
                    displayName={action.entry.displayName}
                    accentColor={action.entry.accentColor}
                    iconClassName="size-4"
                  />
                  <span className="truncate">{action.label}</span>
                </MenuItem>
              );
              return disabled && reason ? (
                <DisabledReasonTooltip key={action.value} reason={reason} trigger={menuItem} />
              ) : (
                menuItem
              );
            })}
          </>
        ) : null}
        <MenuSeparator />
        {unavailableReason ? (
          <DisabledReasonTooltip
            reason={unavailableReason}
            trigger={
              <MenuItem className="data-disabled:pointer-events-auto" disabled>
                <PlusIcon className="size-4" />
                Add custom action…
              </MenuItem>
            }
          />
        ) : (
          <MenuItem onClick={onAddCustomAction}>
            <PlusIcon className="size-4" />
            Add custom action…
          </MenuItem>
        )}
      </MenuPopup>
    </Menu>
  );
});

export default ChatHeaderPanelMenu;
