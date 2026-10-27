import { cloneElement, isValidElement, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vite-plus/test";

// Render tooltip popups inline, marked, so a test can tell the text a reader
// sees from the text kept behind the tooltip.
vi.mock("../ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render }: { render: ReactElement }) => cloneElement(render),
  TooltipPopup: ({ children }: { children: ReactNode }) => (
    <span data-tooltip-popup="">{children}</span>
  ),
}));

import { EnvironmentId } from "@coddy/contracts";

import { SidebarProjectAvailability } from "./SidebarProjectAvailability";
import type { EnvironmentConnectionPresentation } from "@coddy/client-runtime/connection";

import type { SidebarProjectAvailabilityView } from "../Sidebar.logic";

const ENVIRONMENT_ID = EnvironmentId.make("environment-1");

function view(
  kind:
    | "available"
    | "empty-confirmed"
    | "loading"
    | "degraded"
    | "storage-changed"
    | "recovery-required"
    | "unavailable"
    | "configuration-error",
): SidebarProjectAvailabilityView {
  switch (kind) {
    case "available":
    case "empty-confirmed":
    case "loading":
      return { kind, environmentId: null, error: null };
    case "degraded":
    case "storage-changed":
    case "recovery-required":
    case "unavailable":
    case "configuration-error":
      return {
        kind,
        environmentId: ENVIRONMENT_ID,
        error: null,
        hasCachedProjects: kind === "degraded",
      };
  }
}

function render(
  kind: Parameters<typeof view>[0],
  actions: { readonly showRetry: boolean; readonly showConnectionSettings: boolean } = {
    showRetry: true,
    showConnectionSettings: true,
  },
) {
  return renderToStaticMarkup(
    <SidebarProjectAvailability
      view={view(kind)}
      showRetry={actions.showRetry}
      showConnectionSettings={actions.showConnectionSettings}
      onRetry={vi.fn()}
      onOpenSettings={vi.fn()}
      onViewDiagnostics={vi.fn()}
      onAdoptStorage={vi.fn()}
    />,
  );
}

describe("SidebarProjectAvailability", () => {
  it("uses the genuine empty copy only for an authoritative empty catalog", () => {
    expect(render("empty-confirmed")).toContain("No projects yet");
    // UI.md: muted text uses the solid token, never an alpha-reduced one.
    expect(render("empty-confirmed")).not.toContain("text-muted-foreground/");
    for (const kind of [
      "loading",
      "degraded",
      "storage-changed",
      "recovery-required",
      "unavailable",
      "configuration-error",
    ] as const) {
      expect(render(kind)).not.toContain("No projects yet");
    }
  });

  it.each([
    ["loading", "Project data is still loading"],
    ["degraded", "Showing cached projects"],
    ["storage-changed", "Project data location changed"],
    ["recovery-required", "Project data needs recovery"],
    ["unavailable", "Projects are unavailable"],
    ["configuration-error", "Project data configuration needs attention"],
  ] as const)("renders honest %s copy", (kind, copy) => {
    expect(render(kind)).toContain(copy);
  });

  it("wires retry, settings, diagnostics, and explicit storage adoption actions", () => {
    const onRetry = vi.fn();
    const onOpenSettings = vi.fn();
    const onViewDiagnostics = vi.fn();
    const onAdoptStorage = vi.fn();
    const element = SidebarProjectAvailability({
      view: view("storage-changed"),
      showRetry: true,
      showConnectionSettings: true,
      onRetry,
      onOpenSettings,
      onViewDiagnostics,
      onAdoptStorage,
    });
    if (element === null || typeof element !== "object" || !("props" in element)) {
      throw new Error("Expected the availability component to render actions.");
    }
    const children = Array.isArray(element.props.children)
      ? element.props.children
      : [element.props.children];
    const actions = children.flatMap((child: any) =>
      child?.props?.children === undefined
        ? []
        : Array.isArray(child.props.children)
          ? child.props.children
          : [child.props.children],
    );
    for (const action of actions) {
      if (action?.props?.children === "Retry") action.props.onClick();
      if (action?.props?.children === "Settings") action.props.onClick();
      if (action?.props?.children === "Diagnostics") action.props.onClick();
      if (action?.props?.children === "Use this data location") action.props.onClick();
    }
    expect(onRetry).toHaveBeenCalledWith(ENVIRONMENT_ID);
    expect(onOpenSettings).toHaveBeenCalledOnce();
    expect(onViewDiagnostics).toHaveBeenCalledOnce();
    expect(onAdoptStorage).toHaveBeenCalledWith(ENVIRONMENT_ID);
  });

  it("renders nothing while live projects are available", () => {
    expect(render("available")).toBe("");
  });

  it("renders only explicitly permitted recovery actions", () => {
    const desktopRemote = render("unavailable", {
      showRetry: false,
      showConnectionSettings: false,
    });
    expect(desktopRemote).toContain("Projects are unavailable");
    expect(desktopRemote).toContain("Diagnostics");
    expect(desktopRemote).not.toContain("Retry");
    expect(desktopRemote).not.toContain("Settings");

    const desktopLocal = render("unavailable", {
      showRetry: true,
      showConnectionSettings: false,
    });
    expect(desktopLocal).toContain("Retry");
    expect(desktopLocal).not.toContain("Settings");

    const browserRemote = render("unavailable", {
      showRetry: true,
      showConnectionSettings: true,
    });
    expect(browserRemote).toContain("Retry");
    expect(browserRemote).toContain("Settings");
  });

  it("labels retained rows as cached while storage adoption is blocked", () => {
    const markup = renderToStaticMarkup(
      <SidebarProjectAvailability
        view={{
          kind: "storage-changed",
          environmentId: ENVIRONMENT_ID,
          error: null,
          hasCachedProjects: true,
        }}
        showRetry
        showConnectionSettings
        onRetry={vi.fn()}
        onOpenSettings={vi.fn()}
        onViewDiagnostics={vi.fn()}
        onAdoptStorage={vi.fn()}
      />,
    );
    expect(markup).toContain("Cached projects remain visible");
  });

  describe("an environment whose connection is down", () => {
    const TOOLTIP = /<span data-tooltip-popup="">([^<]*)<\/span>/u;
    const SCREEN_READER_ONLY = /<span[^>]*class="sr-only"[^>]*>[^<]*<\/span>/gu;

    function renderNotice(
      view: SidebarProjectAvailabilityView,
      connection: EnvironmentConnectionPresentation,
      options: { readonly showOpenRemoteServers?: boolean; readonly showRetry?: boolean } = {},
    ) {
      const markup = renderToStaticMarkup(
        <SidebarProjectAvailability
          view={view}
          environment={{ label: "devbox", connection }}
          showRetry={options.showRetry ?? false}
          showConnectionSettings={false}
          showOpenRemoteServers={options.showOpenRemoteServers ?? true}
          onRetry={vi.fn()}
          onOpenSettings={vi.fn()}
          onViewDiagnostics={vi.fn()}
          onAdoptStorage={vi.fn()}
        />,
      );
      return {
        visible: markup.replace(TOOLTIP, "").replace(SCREEN_READER_ONLY, ""),
        tooltip: TOOLTIP.exec(markup)?.[1] ?? null,
      };
    }

    it("names it, keeps the connection's reason behind a tooltip, and offers Open Remote Servers", () => {
      const { visible, tooltip } = renderNotice(
        {
          kind: "configuration-error",
          environmentId: ENVIRONMENT_ID,
          error: null,
          hasCachedProjects: true,
        },
        {
          phase: "error",
          error:
            "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
          traceId: null,
        },
      );

      expect(visible).toContain("devbox is not connected.");
      expect(visible).toContain("Cached projects remain visible.");
      expect(visible).not.toContain("rejected a new pairing credential");
      expect(visible).not.toContain("Project data configuration needs attention");
      expect(tooltip).toBe(
        "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
      );
      expect(visible).toContain("Open Remote Servers");
      expect(visible).toContain("Diagnostics");
      expect(visible).not.toContain(">Retry<");
    });

    it("takes a retrying timeout's reason from the connection, which the shell clears", () => {
      const { visible, tooltip } = renderNotice(
        {
          kind: "degraded",
          environmentId: ENVIRONMENT_ID,
          error: null,
          hasCachedProjects: true,
        },
        {
          phase: "reconnecting",
          error:
            "The remote host did not issue a pairing credential within 30 seconds. Check the connection; Coddy Studio keeps trying.",
          traceId: null,
        },
      );

      expect(visible).toContain("devbox is not connected.");
      expect(visible).toContain("Cached projects remain visible.");
      expect(visible).not.toContain("did not issue a pairing credential");
      expect(tooltip).toBe(
        "The remote host did not issue a pairing credential within 30 seconds. Check the connection; Coddy Studio keeps trying.",
      );
    });

    it("offers Retry instead of Open Remote Servers where the client reconnects in place", () => {
      const { visible } = renderNotice(
        {
          kind: "unavailable",
          environmentId: ENVIRONMENT_ID,
          error: null,
          hasCachedProjects: false,
        },
        { phase: "offline", error: null, traceId: null },
        { showOpenRemoteServers: false, showRetry: true },
      );

      expect(visible).toContain("devbox is not connected.");
      expect(visible).toContain(">Retry<");
      expect(visible).not.toContain("Open Remote Servers");
    });

    it("keeps the project-data copy and a visible error while the connection is up", () => {
      const { visible, tooltip } = renderNotice(
        {
          kind: "degraded",
          environmentId: ENVIRONMENT_ID,
          error: "Could not synchronize environment data.",
          hasCachedProjects: true,
        },
        { phase: "connected", error: null, traceId: null },
      );

      expect(visible).toContain("Showing cached projects");
      expect(visible).toContain("Could not synchronize environment data.");
      expect(visible).not.toContain("is not connected");
      expect(visible).not.toContain("Open Remote Servers");
      expect(tooltip).toBeNull();
    });

    it("opens Remote Servers from its action", () => {
      const onOpenSettings = vi.fn();
      const element = SidebarProjectAvailability({
        view: {
          kind: "configuration-error",
          environmentId: ENVIRONMENT_ID,
          error: null,
          hasCachedProjects: false,
        },
        environment: {
          label: "devbox",
          connection: {
            phase: "error",
            error:
              "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
            traceId: null,
          },
        },
        showRetry: false,
        showConnectionSettings: false,
        showOpenRemoteServers: true,
        onRetry: vi.fn(),
        onOpenSettings,
        onViewDiagnostics: vi.fn(),
        onAdoptStorage: vi.fn(),
      });
      const open = findElementByText(element, "Open Remote Servers");

      expect(open).not.toBeNull();
      (open!.props as { onClick: () => void }).onClick();
      expect(onOpenSettings).toHaveBeenCalledOnce();
    });
  });
});

/** The first element in `node`'s tree whose children are exactly `text`. */
function findElementByText(node: ReactNode, text: string): ReactElement | null {
  if (Array.isArray(node)) {
    for (const child of node) {
      const found = findElementByText(child, text);
      if (found) return found;
    }
    return null;
  }
  if (!isValidElement(node)) return null;
  const children = (node.props as { children?: ReactNode }).children;
  if (children === text) return node;
  return findElementByText(children, text);
}
