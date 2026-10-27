import type { OrchestrationThreadActivity } from "@coddy/contracts";
import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

vi.mock("../ui/popover", () => ({
  Popover: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  PopoverTrigger: ({ render, children }: { render?: ReactNode; children?: ReactNode }) => (
    <>{render ?? children}</>
  ),
  PopoverPopup: ({ children }: { children: ReactNode }) => (
    <section role="dialog" aria-labelledby="mcp-title">
      {children}
    </section>
  ),
  PopoverTitle: ({ children }: { children: ReactNode }) => <h2 id="mcp-title">{children}</h2>,
}));

vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => <div>{children}</div>,
  TooltipTrigger: ({ render }: { render: ReactNode }) => <>{render}</>,
  TooltipPopup: ({ children }: { children: ReactNode }) => <section>{children}</section>,
}));

import { deriveMcpStatusSnapshot, McpStatusPopover } from "./McpStatusPopover";

function activity(
  payload: unknown,
  overrides: Partial<OrchestrationThreadActivity> = {},
): OrchestrationThreadActivity {
  return {
    id: "activity-1" as OrchestrationThreadActivity["id"],
    tone: "info",
    kind: "provider-event",
    summary: "mcp.status.updated",
    payload,
    turnId: null,
    createdAt: "2026-08-01T12:00:00.000Z",
    ...overrides,
  };
}

describe("deriveMcpStatusSnapshot", () => {
  it("recognizes MCP status events with a plain summary", () => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity(
            {
              eventType: "mcp.status.updated",
              providerInstanceId: "codex_work",
              servers: [{ name: "context7", state: "connected" }],
            },
            { summary: "Tool connections updated" },
          ),
        ],
        "codex_work",
        true,
      ),
    ).toEqual({ servers: [{ name: "context7", state: "connected", detail: null }] });
  });

  it("recognizes legacy MCP status rows without an event type", () => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity({
            providerInstanceId: "codex_work",
            servers: [{ name: "legacy", state: "connected" }],
          }),
        ],
        "codex_work",
        true,
      ),
    ).toEqual({ servers: [{ name: "legacy", state: "connected", detail: null }] });
  });

  it.each(["Tool connections updated", "mcp.status.updated"])(
    "ignores unrelated events whose summary is %s",
    (summary) => {
      expect(
        deriveMcpStatusSnapshot(
          [
            activity({
              providerInstanceId: "codex_work",
              servers: [{ name: "older", state: "connected" }],
            }),
            activity(
              {
                eventType: "provider.note",
                providerInstanceId: "codex_work",
                servers: [{ name: "unrelated", state: "error" }],
              },
              { summary },
            ),
          ],
          "codex_work",
          true,
        ),
      ).toEqual({ servers: [{ name: "older", state: "connected", detail: null }] });
    },
  );

  it.each([null, ""])("does not treat a present %j event type as missing", (eventType) => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity({
            eventType,
            providerInstanceId: "codex_work",
            servers: [{ name: "unrelated", state: "connected" }],
          }),
        ],
        "codex_work",
        true,
      ),
    ).toEqual({ servers: [] });
  });

  it("uses the newest valid snapshot for the active provider instance", () => {
    const snapshot = deriveMcpStatusSnapshot(
      [
        activity({
          providerInstanceId: "codex_work",
          servers: [{ name: "old", state: "starting" }],
        }),
        activity({
          providerInstanceId: "codex_work",
          servers: [{ name: "new", state: "connected" }],
        }),
      ],
      "codex_work",
      true,
    );

    expect(snapshot).toEqual({
      servers: [{ name: "new", state: "connected", detail: null }],
    });
  });

  it("ignores newer snapshots from another provider instance", () => {
    const snapshot = deriveMcpStatusSnapshot(
      [
        activity({
          providerInstanceId: "codex_work",
          servers: [{ name: "mine", state: "connected" }],
        }),
        activity({
          providerInstanceId: "codex_personal",
          servers: [{ name: "other", state: "error" }],
        }),
      ],
      "codex_work",
      true,
    );

    expect(snapshot).toEqual({
      servers: [{ name: "mine", state: "connected", detail: null }],
    });
  });

  it("returns awaiting when the active instance has not sent a valid snapshot", () => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity({
            providerInstanceId: "codex_work",
            servers: [{ name: "bad", state: "unknown" }],
          }),
        ],
        "codex_work",
        true,
      ),
    ).toEqual({ servers: [] });
  });

  it("skips a malformed newer snapshot instead of masking an older valid one", () => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity({
            providerInstanceId: "codex_work",
            servers: [{ name: "older", state: "connected" }],
          }),
          activity({
            providerInstanceId: "codex_work",
            servers: [
              { name: "newer", state: "starting" },
              { name: "bad", state: "unknown" },
            ],
          }),
        ],
        "codex_work",
        true,
      ),
    ).toEqual({ servers: [{ name: "older", state: "connected", detail: null }] });
  });

  it("keeps an explicitly empty latest snapshot as valid awaiting state", () => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity({
            providerInstanceId: "codex_work",
            servers: [{ name: "older", state: "connected" }],
          }),
          activity({ providerInstanceId: "codex_work", servers: [] }),
        ],
        "codex_work",
        true,
      ),
    ).toEqual({ servers: [] });
  });

  it("clears stale connected and starting rows when the runtime is not live", () => {
    expect(
      deriveMcpStatusSnapshot(
        [
          activity({
            providerInstanceId: "codex_work",
            servers: [
              { name: "connected", state: "connected" },
              { name: "starting", state: "starting" },
              { name: "auth", state: "needs-auth" },
            ],
          }),
        ],
        "codex_work",
        false,
      ),
    ).toEqual({
      servers: [
        { name: "connected", state: "disconnected", detail: null },
        { name: "starting", state: "disconnected", detail: null },
        { name: "auth", state: "needs-auth", detail: null },
      ],
    });
  });
});

describe("McpStatusPopover", () => {
  it("renders every MCP state with an accessible status and wrapped detail", () => {
    const markup = renderToStaticMarkup(
      <McpStatusPopover
        supported
        snapshot={{
          servers: [
            { name: "connected", state: "connected", detail: null },
            { name: "starting", state: "starting", detail: "Starting connection" },
            { name: "auth", state: "needs-auth", detail: "Sign in to continue" },
            { name: "offline", state: "disconnected", detail: null },
            { name: "failed", state: "error", detail: "Connection failed" },
          ],
        }}
      />,
    );

    expect(markup).toContain('aria-label="MCP servers"');
    expect(markup).toContain('<section role="dialog" aria-labelledby="mcp-title">');
    expect(markup).toContain('<h2 id="mcp-title">MCPs</h2>');
    expect(markup).toContain("MCPs");
    expect(markup).toContain('role="status"');
    expect(markup).toContain("Connected");
    expect(markup).toContain("Starting");
    expect(markup).toContain("Needs authentication");
    expect(markup).toContain("Disconnected");
    expect(markup).toContain("Error");
    expect(markup).toContain('title="connected"');
    expect(markup).toContain("Sign in to continue");
  });

  it("shows a neutral awaiting state before the first snapshot", () => {
    expect(
      renderToStaticMarkup(<McpStatusPopover supported snapshot={{ servers: [] }} />),
    ).toContain("Awaiting MCP status");
  });

  it("renders a disabled tooltip-only control when MCP status is unavailable", () => {
    const markup = renderToStaticMarkup(
      <McpStatusPopover supported={false} snapshot={{ servers: [] }} />,
    );

    expect(markup).toContain('aria-disabled="true"');
    expect(markup).toContain("MCP servers unavailable");
    expect(markup).toContain("MCP status is not available for this provider.");
    expect(markup).not.toContain('role="dialog"');
    expect(markup).not.toContain("Awaiting MCP status");
  });
});
