// @vitest-environment happy-dom

import { DEFAULT_SERVER_SETTINGS, EnvironmentId, ThreadId } from "@coddy/contracts";
import { act, type ComponentProps, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  describe,
  expect,
  it,
  vi,
} from "vite-plus/test";

const harness = vi.hoisted(() => ({
  primaryEnvironmentId: "environment-primary" as EnvironmentId | null,
  panelProps: null as Record<string, unknown> | null,
  gitProps: [] as Array<Record<string, unknown>>,
  gitMounts: 0,
  gitUnmounts: 0,
  projectControllerProps: [] as Array<Record<string, unknown>>,
  expandedControllers: [] as unknown[],
  menuControllers: [] as unknown[],
  dialogControllers: [] as unknown[],
  openInControllerProps: [] as Array<Record<string, unknown>>,
  expandedOpenInControllers: [] as unknown[],
  menuOpenInControllers: [] as unknown[],
  openFavoriteEditor: vi.fn(),
  iconControlProps: [] as Array<Record<string, unknown>>,
}));

vi.mock("../../state/environments", () => ({
  usePrimaryEnvironmentId: () => harness.primaryEnvironmentId,
}));

vi.mock("./ChatHeaderPanelMenu", () => ({
  ChatHeaderPanelMenu: (props: Record<string, unknown>) => {
    harness.panelProps = props;
    return (
      <button
        type="button"
        aria-label="New panel"
        onClick={props["onAddCustomAction"] as () => void}
      >
        New panel
      </button>
    );
  },
}));

vi.mock("../ProjectScriptsControl", async () => {
  const React = await import("react");
  return {
    useProjectScriptsController: (props: Record<string, unknown>) => {
      harness.projectControllerProps.push(props);
      const scripts = props["scripts"] as unknown[];
      const [dialogOpen, setDialogOpen] = React.useState(false);
      const [name, setName] = React.useState("");
      return {
        scripts,
        primaryScript: scripts[0] ?? null,
        dialogOpen,
        name,
        setName,
        openAddDialog: () => setDialogOpen(true),
        openEditDialog: vi.fn(),
        runScript: vi.fn(),
      };
    },
    ProjectScriptsExpandedActions: ({ controller }: { controller: Record<string, unknown> }) => {
      harness.expandedControllers.push(controller);
      return controller["primaryScript"] ? (
        <div role="group" aria-label="Project scripts">
          <button type="button" aria-label="Script actions">
            Script actions
          </button>
        </div>
      ) : null;
    },
    ProjectScriptsMenuItems: ({ controller }: { controller: Record<string, unknown> }) => {
      harness.menuControllers.push(controller);
      return (
        <>
          {(controller["scripts"] as Array<{ name: string }>).map((script) => (
            <button type="button" key={script.name}>
              {script.name}
              <kbd>Mod+R</kbd>
              <span aria-label={`Edit ${script.name}`} />
            </button>
          ))}
          <button type="button" onClick={controller["openAddDialog"] as () => void}>
            Add action
          </button>
        </>
      );
    },
    ProjectScriptsDialogs: ({ controller }: { controller: Record<string, unknown> }) => {
      harness.dialogControllers.push(controller);
      if (!controller["dialogOpen"]) return null;
      return (
        <form aria-label="Add Action">
          <input
            aria-label="Action name"
            value={controller["name"] as string}
            onChange={(event) =>
              (controller["setName"] as (value: string) => void)(event.currentTarget.value)
            }
          />
        </form>
      );
    },
  };
});

vi.mock("./OpenInPicker", async () => {
  const React = await import("react");
  function useOpenInEditorController(props: Record<string, unknown>) {
    harness.openInControllerProps.push(props);
    React.useEffect(() => {
      if (props["enableShortcut"] === false) return;
      const handler = (event: KeyboardEvent) => {
        if (event.key === "o") harness.openFavoriteEditor();
      };
      window.addEventListener("keydown", handler);
      return () => window.removeEventListener("keydown", handler);
    }, [props["enableShortcut"]]);
    return {
      options: (props["availableEditors"] as string[]).map((editor) => ({
        label: editor === "vscode" ? "VS Code" : editor,
        value: editor,
      })),
    };
  }
  function OpenInExpandedActions({ controller }: { controller: unknown }) {
    harness.expandedOpenInControllers.push(controller);
    return (
      <div role="group" aria-label="Open in editor">
        <button type="button" aria-label="Copy options">
          Copy options
        </button>
      </div>
    );
  }
  return {
    useOpenInEditorController,
    OpenInExpandedActions,
    OpenInPicker: (props: Record<string, unknown>) => (
      <OpenInExpandedActions controller={useOpenInEditorController(props)} />
    ),
    OpenInMenuItems: ({ controller }: { controller: Record<string, unknown> }) => {
      harness.menuOpenInControllers.push(controller);
      return (
        <>
          {(controller["options"] as Array<{ label: string; value: string }>).map((option) => (
            <button type="button" key={option.value}>
              {option.label}
            </button>
          ))}
        </>
      );
    },
  };
});

vi.mock("../ui/button", () => ({
  Button: ({ children, ...props }: ComponentProps<"button">) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
}));

vi.mock("../CenterHeaderIconButton", () => ({
  CenterHeaderIconButton: ({ children, ...props }: Record<string, unknown>) => {
    harness.iconControlProps.push(props);
    return (
      <button type="button" data-center-header-icon-control {...props}>
        {children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children: ReactNode }) => <div data-menu-root>{children}</div>,
  MenuTrigger: ({ children, render }: { children: ReactNode; render: ReactNode }) => (
    <>
      {render}
      {children}
    </>
  ),
  MenuPopup: ({ children }: { children: ReactNode }) => <div data-menu-popup>{children}</div>,
  MenuSeparator: () => <hr />,
}));

vi.mock("../GitActionsControl", async () => {
  const React = await import("react");
  return {
    default: (props: Record<string, unknown>) => {
      harness.gitProps.push(props);
      React.useEffect(() => {
        harness.gitMounts += 1;
        return () => {
          harness.gitUnmounts += 1;
        };
      }, []);
      return <div data-testid="git-actions" />;
    },
  };
});

import { ChatHeaderActions } from "./ChatHeaderActions";

const environmentId = EnvironmentId.make("environment-primary");
const projectScript = {
  id: "dev",
  name: "Dev",
  command: "vp dev",
  icon: "play" as const,
  runOnWorktreeCreate: false,
};

function props(
  overrides: Partial<ComponentProps<typeof ChatHeaderActions>> = {},
): ComponentProps<typeof ChatHeaderActions> {
  return {
    density: "expanded",
    activeThreadEnvironmentId: environmentId,
    activeThreadId: ThreadId.make("thread-1"),
    activeProjectName: "Project",
    openInCwd: "/repo",
    activeProjectScripts: [projectScript],
    preferredScriptId: null,
    keybindings: {} as never,
    availableEditors: ["vscode"],
    reserveTitlebarControls: true,
    gitCwd: "/repo",
    providerStatuses: [],
    settings: {
      providerInstances: DEFAULT_SERVER_SETTINGS.providerInstances,
      providers: DEFAULT_SERVER_SETTINGS.providers,
      providerSessionDefaults: DEFAULT_SERVER_SETTINGS.providerSessionDefaults,
    },
    canCreatePanel: false,
    onCreateChatPanel: vi.fn(),
    onReopenChatPanel: vi.fn(),
    onOpenTerminalPanel: vi.fn(),
    onOpenProviderTerminalPanel: vi.fn(),
    onRunProjectScript: vi.fn(),
    onAddProjectScript: vi.fn(),
    onUpdateProjectScript: vi.fn(),
    onDeleteProjectScript: vi.fn(),
    ...overrides,
  };
}

const mountedRoots: Root[] = [];

async function mount(overrides: Partial<ComponentProps<typeof ChatHeaderActions>> = {}) {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedRoots.push(root);
  await act(async () => root.render(<ChatHeaderActions {...props(overrides)} />));
  return {
    container,
    rerender: async (next: Partial<ComponentProps<typeof ChatHeaderActions>>) => {
      await act(async () => root.render(<ChatHeaderActions {...props(next)} />));
    },
  };
}

beforeAll(() => {
  (
    globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean }
  ).IS_REACT_ACT_ENVIRONMENT = true;
});

afterAll(() => {
  delete (globalThis as typeof globalThis & { IS_REACT_ACT_ENVIRONMENT?: boolean })
    .IS_REACT_ACT_ENVIRONMENT;
});

beforeEach(() => {
  harness.primaryEnvironmentId = environmentId;
  harness.panelProps = null;
  harness.gitProps.length = 0;
  harness.gitMounts = 0;
  harness.gitUnmounts = 0;
  harness.projectControllerProps.length = 0;
  harness.expandedControllers.length = 0;
  harness.menuControllers.length = 0;
  harness.dialogControllers.length = 0;
  harness.openInControllerProps.length = 0;
  harness.expandedOpenInControllers.length = 0;
  harness.menuOpenInControllers.length = 0;
  harness.openFavoriteEditor.mockReset();
  harness.iconControlProps.length = 0;
});

afterEach(async () => {
  for (const root of mountedRoots.splice(0)) await act(async () => root.unmount());
  document.body.replaceChildren();
});

describe("ChatHeaderActions rendering", () => {
  it("keeps New panel and expanded controls at wide density", () => {
    const markup = renderToStaticMarkup(<ChatHeaderActions {...props()} density="expanded" />);

    expect(markup).toContain('aria-label="New panel"');
    expect(markup).toContain('aria-label="Project scripts"');
    expect(markup).toContain('aria-label="Open in editor"');
    expect(markup).not.toContain('aria-label="More workspace actions"');
    expect(markup.match(/data-testid="git-actions"/g)).toHaveLength(1);
    expect(markup).toContain("gap-2");
    expect(markup).toContain("@3xl/header-actions:gap-3");
  });

  it("keeps New panel and one overflow trigger at compact density", () => {
    const markup = renderToStaticMarkup(<ChatHeaderActions {...props()} density="compact" />);

    expect(markup).toContain('aria-label="New panel"');
    expect(markup.match(/aria-label="More workspace actions"/g)).toHaveLength(1);
    expect(markup).not.toContain('aria-label="Script actions"');
    expect(markup).not.toContain('aria-label="Copy options"');
    expect(markup.match(/data-testid="git-actions"/g)).toHaveLength(1);
    expect(harness.iconControlProps).toEqual([
      expect.objectContaining({ "aria-label": "More workspace actions" }),
    ]);
    expect(markup).toContain('data-center-header-icon-control="true"');
    expect(markup).toContain("gap-1");
  });

  it("renders flat compact script and editor actions with one non-empty separator", () => {
    const markup = renderToStaticMarkup(<ChatHeaderActions {...props()} density="compact" />);

    expect(markup).toContain("Dev");
    expect(markup).toContain("Mod+R");
    expect(markup).toContain('aria-label="Edit Dev"');
    expect(markup).toContain("Add action");
    expect(markup).toContain("VS Code");
    expect(markup.match(/<hr/g)).toHaveLength(1);
    expect(markup.match(/data-menu-root/g)).toHaveLength(1);
  });

  it("omits unavailable compact sections, separators, and an empty overflow", () => {
    const noEditorMarkup = renderToStaticMarkup(
      <ChatHeaderActions {...props({ availableEditors: [] })} density="compact" />,
    );
    expect(noEditorMarkup).toContain("Add action");
    expect(noEditorMarkup).not.toContain("<hr");

    const noProjectMarkup = renderToStaticMarkup(
      <ChatHeaderActions
        {...props({ activeProjectName: undefined, activeProjectScripts: undefined })}
        density="compact"
      />,
    );
    expect(noProjectMarkup).not.toContain('aria-label="More workspace actions"');
    expect(noProjectMarkup).not.toContain("<hr");
  });

  it("owns one project and editor controller across each presentation", () => {
    renderToStaticMarkup(<ChatHeaderActions {...props()} density="expanded" />);

    expect(harness.projectControllerProps).toHaveLength(1);
    expect(harness.openInControllerProps).toHaveLength(1);
    expect(harness.expandedControllers[0]).toBe(harness.dialogControllers[0]);
    expect(harness.expandedOpenInControllers).toHaveLength(1);

    harness.projectControllerProps.length = 0;
    harness.openInControllerProps.length = 0;
    harness.dialogControllers.length = 0;
    renderToStaticMarkup(<ChatHeaderActions {...props()} density="compact" />);

    expect(harness.projectControllerProps).toHaveLength(1);
    expect(harness.openInControllerProps).toHaveLength(1);
    expect(harness.menuControllers[0]).toBe(harness.dialogControllers[0]);
    expect(harness.menuOpenInControllers).toHaveLength(1);
  });

  it("keeps dialog form state and one hidden Git lifecycle across density changes", async () => {
    const mounted = await mount({ density: "expanded" });
    const newPanel = mounted.container.querySelector<HTMLButtonElement>('[aria-label="New panel"]');
    await act(async () => newPanel?.click());
    const name = mounted.container.querySelector<HTMLInputElement>('[aria-label="Action name"]');
    expect(name).not.toBeNull();
    await act(async () => {
      if (!name) return;
      const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
      setter?.call(name, "Release");
      name.dispatchEvent(new Event("input", { bubbles: true }));
    });

    await mounted.rerender({ density: "compact" });

    expect(mounted.container.querySelector('[aria-label="More workspace actions"]')).not.toBeNull();
    expect(mounted.container.querySelector('[aria-label="Script actions"]')).toBeNull();
    expect(
      mounted.container.querySelector<HTMLInputElement>('[aria-label="Action name"]')?.value,
    ).toBe("Release");
    expect(mounted.container.querySelectorAll('[data-testid="git-actions"]')).toHaveLength(1);
    expect(harness.gitMounts).toBe(1);
    expect(harness.gitUnmounts).toBe(0);
  });

  it("runs one favorite-editor command after changing density", async () => {
    const mounted = await mount({ density: "expanded" });
    await mounted.rerender({ density: "compact" });

    window.dispatchEvent(new KeyboardEvent("keydown", { key: "o" }));

    expect(harness.openFavoriteEditor).toHaveBeenCalledOnce();
  });

  it("uses a consistent inset while preserving the action-region safeguards", () => {
    const reserved = renderToStaticMarkup(
      <ChatHeaderActions {...props()} reserveTitlebarControls density="expanded" />,
    );
    const unreserved = renderToStaticMarkup(
      <ChatHeaderActions {...props()} reserveTitlebarControls={false} density="compact" />,
    );

    expect(reserved).toContain("pr-[4.5rem]");
    expect(unreserved).toContain("pr-2");
    expect(reserved).toContain("shrink-0");
    expect(reserved).toContain("bg-background");
    expect(reserved).toContain("[-webkit-app-region:no-drag]");
  });

  it("keeps the compact action rail intrinsically sized for sibling flex layout", () => {
    const wrapper = document.createElement("div");
    wrapper.innerHTML = renderToStaticMarkup(
      <ChatHeaderActions {...props()} reserveTitlebarControls={false} density="compact" />,
    );
    const actionRail = wrapper.querySelector<HTMLElement>("[data-chat-header-actions]");

    expect(actionRail?.classList.contains("shrink-0")).toBe(true);
    expect(actionRail?.classList.contains("@container/header-actions")).toBe(false);
  });

  it("disables the editor shortcut owner outside the primary environment", () => {
    harness.primaryEnvironmentId = EnvironmentId.make("different");
    renderToStaticMarkup(<ChatHeaderActions {...props()} density="compact" />);

    expect(harness.openInControllerProps[0]).toMatchObject({ enableShortcut: false });
    expect(harness.menuOpenInControllers).toHaveLength(0);
  });

  it("forwards panel actions and the optional draft identity without duplicating Git", () => {
    const onOpenProviderTerminalPanel = vi.fn();
    renderToStaticMarkup(
      <ChatHeaderActions
        {...props({
          density: "compact",
          draftId: "draft-1" as never,
          onOpenProviderTerminalPanel,
        })}
      />,
    );

    expect(harness.panelProps?.["onOpenProviderTerminalPanel"]).toBe(onOpenProviderTerminalPanel);
    expect(harness.gitProps).toHaveLength(1);
    expect(harness.gitProps[0]).toMatchObject({ draftId: "draft-1", hideTrigger: true });
  });

  it("still owns disabled controllers when no project actions are available", () => {
    const markup = renderToStaticMarkup(
      <ChatHeaderActions
        {...props({
          density: "compact",
          activeProjectName: undefined,
          activeProjectScripts: undefined,
          openInCwd: null,
        })}
      />,
    );

    expect(markup).toContain('aria-label="New panel"');
    expect(markup).not.toContain('aria-label="More workspace actions"');
    expect(markup).not.toContain('data-testid="git-actions"');
    expect(harness.projectControllerProps).toHaveLength(1);
    expect(harness.projectControllerProps[0]).toMatchObject({
      enabled: false,
      disabledReason: null,
    });
    expect(harness.openInControllerProps).toHaveLength(1);
    expect(harness.openInControllerProps[0]).toMatchObject({ enableShortcut: false });
  });

  it("uses the shared workspace reason for scripts, panels, editor, and Git actions", () => {
    const reason = "Workspace unavailable. Retry detection or remove it from Coddy Studio.";
    renderToStaticMarkup(
      <ChatHeaderActions {...props({ workspaceUnavailable: reason })} density="expanded" />,
    );

    expect(harness.projectControllerProps[0]).toMatchObject({
      enabled: false,
      disabledReason: reason,
    });
    expect(harness.openInControllerProps[0]).toMatchObject({ enableShortcut: false });
    expect(harness.panelProps).toMatchObject({
      canCreatePanel: false,
      unavailableReason: reason,
    });
    expect(harness.gitProps[0]).toMatchObject({ workspaceUnavailable: reason });
  });
});
