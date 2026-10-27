import type { EnvironmentAvailabilityStatus } from "@coddy/client-runtime/state/shell";
import { useLocation, useNavigate } from "@tanstack/react-router";
import { BotIcon } from "lucide-react";
import { useCallback, useMemo } from "react";

import { useSidebarWorkspaceMetaStore } from "../../sidebarWorkspaceMetaStore";
import { useProjects, useThreadShells } from "../../state/entities";
import { useEnvironments } from "../../state/environments";
import { useEnvironmentShellSummary } from "../../state/shell";
import { SidebarGroup, SidebarMenu, SidebarMenuButton, SidebarMenuItem } from "../ui/sidebar";
import { buildAgentRows, countUnreadAgentRows } from "./agentsSection.logic";
import { useAgentsUnread } from "./useAgentsUnread";

export function AgentsNavRow() {
  const navigate = useNavigate();
  const pathname = useLocation({ select: (location) => location.pathname });
  const shells = useThreadShells();
  const projects = useProjects();
  const { environments } = useEnvironments();
  const availability = useEnvironmentShellSummary().statuses;
  const unreadThreadKeys = useSidebarWorkspaceMetaStore((state) => state.unreadThreadKeys);
  const rows = useMemo(
    () =>
      buildAgentRows({
        shells,
        projectTitleById: new Map<string, string>(
          projects.map((project) => [project.id, project.title]),
        ),
        environmentLabelById: new Map<string, string>(
          environments.map((environment) => [environment.environmentId, environment.label]),
        ),
        availabilityByEnvironmentId: new Map<string, EnvironmentAvailabilityStatus>(
          availability.map(({ environmentId, status }) => [environmentId, status]),
        ),
      }),
    [availability, environments, projects, shells],
  );
  useAgentsUnread(rows);
  const unreadCount = useMemo(
    () => countUnreadAgentRows(rows, unreadThreadKeys),
    [rows, unreadThreadKeys],
  );
  const handleClick = useCallback(() => {
    void navigate({ to: "/agents" });
  }, [navigate]);

  return (
    <SidebarGroup className="px-2 py-1">
      <SidebarMenu>
        <SidebarMenuItem>
          <SidebarMenuButton
            size="sm"
            isActive={pathname === "/agents"}
            className="gap-2 px-2 py-1.5 text-foreground/80 hover:bg-accent hover:text-foreground"
            data-testid="agents-nav-row"
            aria-current={pathname === "/agents" ? "page" : undefined}
            onClick={handleClick}
          >
            <BotIcon className="size-4 text-foreground/60" aria-hidden />
            <span className="flex-1 truncate text-left text-[13px] font-medium">Agents</span>
            {unreadCount > 0 ? (
              <span className="rounded-full bg-muted px-1.5 py-0.5 text-xs font-medium tabular-nums text-muted-foreground">
                {unreadCount}
              </span>
            ) : null}
          </SidebarMenuButton>
        </SidebarMenuItem>
      </SidebarMenu>
    </SidebarGroup>
  );
}
