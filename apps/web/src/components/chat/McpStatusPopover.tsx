import type { OrchestrationThreadActivity } from "@coddy/contracts";
import { CircleIcon, PlugIcon } from "lucide-react";

import { cn } from "~/lib/utils";
import { Button } from "../ui/button";
import { Popover, PopoverPopup, PopoverTitle, PopoverTrigger } from "../ui/popover";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../ui/tooltip";

type McpServerState = "connected" | "starting" | "needs-auth" | "disconnected" | "error";

export type McpStatusSnapshot = {
  readonly servers: ReadonlyArray<{
    readonly name: string;
    readonly state: McpServerState;
    readonly detail: string | null;
  }>;
};

const mcpServerStates = new Set<McpServerState>([
  "connected",
  "starting",
  "needs-auth",
  "disconnected",
  "error",
]);

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : null;
}

function parseSnapshot(payload: unknown): McpStatusSnapshot | null {
  const record = asRecord(payload);
  if (!record || !Array.isArray(record.servers)) return null;

  const servers: McpStatusSnapshot["servers"][number][] = [];
  for (const value of record.servers) {
    const server = asRecord(value);
    if (!server) return null;
    const name = typeof server.name === "string" ? server.name.trim() : "";
    const state = server.state;
    if (!name || typeof state !== "string" || !mcpServerStates.has(state as McpServerState)) {
      return null;
    }
    const detail = server.detail;
    if (detail !== undefined && (typeof detail !== "string" || !detail.trim())) return null;
    servers.push({ name, state: state as McpServerState, detail: detail ?? null });
  }

  return { servers };
}

function isMcpStatusActivity(activity: OrchestrationThreadActivity): boolean {
  const eventType = asRecord(activity.payload)?.eventType;
  return eventType === undefined
    ? activity.summary === "mcp.status.updated"
    : eventType === "mcp.status.updated";
}

export function deriveMcpStatusSnapshot(
  activities: ReadonlyArray<OrchestrationThreadActivity>,
  activeInstanceId: string | null | undefined,
  runtimeLive: boolean,
): McpStatusSnapshot {
  if (!activeInstanceId) return { servers: [] };

  for (let index = activities.length - 1; index >= 0; index -= 1) {
    const activity = activities[index];
    if (!activity || !isMcpStatusActivity(activity)) continue;

    const payload = asRecord(activity.payload);
    if (payload?.providerInstanceId !== activeInstanceId) continue;
    const snapshot = parseSnapshot(payload);
    if (!snapshot) continue;
    if (runtimeLive) return snapshot;
    return {
      servers: snapshot.servers.map((server) =>
        server.state === "connected" || server.state === "starting"
          ? { ...server, state: "disconnected" as const }
          : server,
      ),
    };
  }

  return { servers: [] };
}

const statusPresentation: Record<McpServerState, { label: string; className: string }> = {
  connected: { label: "Connected", className: "text-success" },
  starting: { label: "Starting", className: "text-blue-500" },
  "needs-auth": { label: "Needs authentication", className: "text-warning" },
  disconnected: { label: "Disconnected", className: "text-muted-foreground" },
  error: { label: "Error", className: "text-destructive" },
};

export function McpStatusPopover({
  supported,
  snapshot,
}: {
  supported: boolean;
  snapshot: McpStatusSnapshot;
}) {
  if (!supported) {
    return (
      <Tooltip>
        <TooltipTrigger
          render={
            <Button
              variant="ghost"
              size="icon-sm"
              type="button"
              aria-disabled="true"
              aria-label="MCP servers unavailable"
            >
              <PlugIcon />
            </Button>
          }
        />
        <TooltipPopup side="top">MCP status is not available for this provider.</TooltipPopup>
      </Tooltip>
    );
  }

  return (
    <Popover>
      <PopoverTrigger
        render={
          <Button variant="ghost" size="icon-sm" type="button" aria-label="MCP servers">
            <PlugIcon />
          </Button>
        }
      />
      <PopoverPopup side="top" align="end" className="w-72 max-w-none p-0">
        <div className="flex flex-col gap-2 p-3">
          <PopoverTitle className="font-medium text-muted-foreground text-xs">MCPs</PopoverTitle>
          {snapshot.servers.length === 0 ? (
            <div className="text-muted-foreground text-xs">Awaiting MCP status</div>
          ) : (
            snapshot.servers.map((server) => {
              const status = statusPresentation[server.state];
              return (
                <div key={server.name} className="flex min-w-0 items-start gap-2 text-xs">
                  <CircleIcon
                    className={cn("mt-0.5 size-3 shrink-0", status.className)}
                    aria-hidden
                  />
                  <div className="min-w-0">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="min-w-0 flex-1 truncate font-medium" title={server.name}>
                        {server.name}
                      </span>
                      <span className="shrink-0 text-muted-foreground" role="status">
                        {status.label}
                      </span>
                    </div>
                    {server.detail ? (
                      <div className="whitespace-pre-wrap break-words text-muted-foreground leading-4">
                        {server.detail}
                      </div>
                    ) : null}
                  </div>
                </div>
              );
            })
          )}
        </div>
      </PopoverPopup>
    </Popover>
  );
}
