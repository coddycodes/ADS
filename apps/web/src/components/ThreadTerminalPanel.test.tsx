import {
  EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  type ResolvedKeybindingsConfig,
  type TerminalLaunchCommand,
} from "@coddy/contracts";
import { scopeThreadRef } from "@coddy/client-runtime/environment";
import { Window } from "happy-dom";
import { type ComponentProps, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const settingsState = vi.hoisted(() => ({ enableTerminalAgentActivity: false }));

// ── Module mocks ────────────────────────────────────────────────────────────
// The panel's `TerminalViewport` child wires xterm + Effect atom state at
// mount time (inside effects). Static server rendering never runs effects, so
// the mocks only need to satisfy the render-time hook calls.

vi.mock("@xterm/xterm", () => ({
  Terminal: class MockTerminal {
    readonly isMockTerminal = true;
  },
}));

vi.mock("@xterm/addon-fit", () => ({
  FitAddon: class MockFitAddon {
    readonly isMockFitAddon = true;
  },
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: () => undefined,
}));

vi.mock("~/localApi", () => ({
  readLocalApi: () => undefined,
}));

vi.mock("../editorPreferences", () => ({
  useOpenInPreferredEditor: () => async () => ({ _tag: "Success" }),
}));

vi.mock("../hooks/useSettings", () => ({
  useEnvironmentSettings: (
    _environmentId: string,
    selector: (settings: { enableTerminalAgentActivity: boolean }) => unknown,
  ) => selector(settingsState),
  usePrimarySettings: (
    selector: (settings: {
      terminal: { webglEnabled: boolean };
      terminalFontPreference: { mode: "bundled" };
      terminalThemePreference: "app" | "dark" | "light";
    }) => unknown,
  ) =>
    selector({
      terminal: { webglEnabled: false },
      terminalFontPreference: { mode: "bundled" },
      terminalThemePreference: "app",
    }),
}));

vi.mock("../state/server", () => ({
  serverEnvironment: {
    configValueAtom: (_environmentId: unknown) => ({ atom: "server-config" }),
  },
}));

vi.mock("../state/preview", () => ({
  previewEnvironment: { open: { atom: "preview-open" } },
}));

vi.mock("../state/terminal", () => ({
  terminalEnvironment: {
    write: { atom: "terminal-write" },
    resize: { atom: "terminal-resize" },
  },
}));

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: () => async () => ({ _tag: "Success" }),
}));

vi.mock("../state/use-atom-query-runner", () => ({
  useAtomQueryRunner: () => async () => ({ _tag: "Success" }),
}));

vi.mock("../state/terminalSessions", () => ({
  useAttachedTerminalSession: () => ({
    buffer: "",
    error: null,
    status: "running",
    version: 0,
  }),
}));

// Base UI popovers are interaction-heavy; replace with static stand-ins that
// keep the trigger element (with its aria-label) and popup label text in the
// markup so assertions can target real behavior.
vi.mock("~/components/ui/popover", () => ({
  Popover: ({ children }: { children?: ReactNode }) => <>{children}</>,
  PopoverTrigger: ({ children, render }: { children?: ReactNode; render?: ReactElement }) => (
    <span data-slot="popover-trigger">
      {render}
      {children}
    </span>
  ),
  PopoverPopup: ({ children }: { children?: ReactNode }) => (
    <span data-slot="popover-popup">{children}</span>
  ),
}));

import ThreadTerminalPanel, {
  TerminalViewport,
  resolveTerminalSelectionActionPosition,
} from "./ThreadTerminalPanel";

const TEST_ENVIRONMENT_ID = EnvironmentId.make("environment-terminal-panel");
const TEST_THREAD_ID = ThreadId.make("thread-terminal-panel");
const TEST_THREAD_REF = scopeThreadRef(TEST_ENVIRONMENT_ID, TEST_THREAD_ID);
const TEST_PROJECT_ID = ProjectId.make("project-terminal-panel");
const EMPTY_KEYBINDINGS: ResolvedKeybindingsConfig = [];

beforeEach(() => {
  settingsState.enableTerminalAgentActivity = false;
});

type PanelProps = ComponentProps<typeof ThreadTerminalPanel>;
type ViewportProps = ComponentProps<typeof TerminalViewport>;

function panelProps(overrides: Partial<PanelProps> = {}): PanelProps {
  return {
    owner: "right-panel",
    threadRef: TEST_THREAD_REF,
    threadId: TEST_THREAD_ID,
    projectId: TEST_PROJECT_ID,
    cwd: "/repo",
    terminalIds: ["term-1"],
    activeTerminalId: "term-1",
    terminalGroups: [],
    activeTerminalGroupId: "",
    focusRequestId: 0,
    onSplitTerminal: () => {},
    onSplitTerminalVertical: () => {},
    onNewTerminal: () => {},
    onActiveTerminalChange: () => {},
    onCloseTerminal: () => {},
    onAddTerminalContext: () => {},
    keybindings: EMPTY_KEYBINDINGS,
    ...overrides,
  };
}

type TerminalToolbarCase = {
  readonly availability: string;
  readonly callbacks: Pick<
    PanelProps,
    "onSplitTerminal" | "onSplitTerminalVertical" | "onNewTerminal"
  >;
  readonly actionLabels: readonly string[];
  readonly floatingSequence: readonly string[];
  readonly sidebarDividerBefore: readonly boolean[];
};

const TERMINAL_TOOLBAR_CASES: readonly TerminalToolbarCase[] = [
  {
    availability: "000",
    callbacks: {
      onSplitTerminal: undefined,
      onSplitTerminalVertical: undefined,
      onNewTerminal: undefined,
    },
    actionLabels: [],
    floatingSequence: [],
    sidebarDividerBefore: [],
  },
  {
    availability: "001",
    callbacks: {
      onSplitTerminal: undefined,
      onSplitTerminalVertical: undefined,
      onNewTerminal: () => {},
    },
    actionLabels: ["New Terminal"],
    floatingSequence: ["New Terminal"],
    sidebarDividerBefore: [false],
  },
  {
    availability: "010",
    callbacks: {
      onSplitTerminal: undefined,
      onSplitTerminalVertical: () => {},
      onNewTerminal: undefined,
    },
    actionLabels: ["Split Terminal Vertically"],
    floatingSequence: ["Split Terminal Vertically"],
    sidebarDividerBefore: [false],
  },
  {
    availability: "011",
    callbacks: {
      onSplitTerminal: undefined,
      onSplitTerminalVertical: () => {},
      onNewTerminal: () => {},
    },
    actionLabels: ["Split Terminal Vertically", "New Terminal"],
    floatingSequence: ["Split Terminal Vertically", "divider", "New Terminal"],
    sidebarDividerBefore: [false, true],
  },
  {
    availability: "100",
    callbacks: {
      onSplitTerminal: () => {},
      onSplitTerminalVertical: undefined,
      onNewTerminal: undefined,
    },
    actionLabels: ["Split Terminal Horizontally"],
    floatingSequence: ["Split Terminal Horizontally"],
    sidebarDividerBefore: [false],
  },
  {
    availability: "101",
    callbacks: {
      onSplitTerminal: () => {},
      onSplitTerminalVertical: undefined,
      onNewTerminal: () => {},
    },
    actionLabels: ["Split Terminal Horizontally", "New Terminal"],
    floatingSequence: ["Split Terminal Horizontally", "divider", "New Terminal"],
    sidebarDividerBefore: [false, true],
  },
  {
    availability: "110",
    callbacks: {
      onSplitTerminal: () => {},
      onSplitTerminalVertical: () => {},
      onNewTerminal: undefined,
    },
    actionLabels: ["Split Terminal Horizontally", "Split Terminal Vertically"],
    floatingSequence: ["Split Terminal Horizontally", "divider", "Split Terminal Vertically"],
    sidebarDividerBefore: [false, true],
  },
  {
    availability: "111",
    callbacks: {
      onSplitTerminal: () => {},
      onSplitTerminalVertical: () => {},
      onNewTerminal: () => {},
    },
    actionLabels: ["Split Terminal Horizontally", "Split Terminal Vertically", "New Terminal"],
    floatingSequence: [
      "Split Terminal Horizontally",
      "divider",
      "Split Terminal Vertically",
      "divider",
      "New Terminal",
    ],
    sidebarDividerBefore: [false, true, true],
  },
];

function renderPanelDocument(overrides: Partial<PanelProps>): Document {
  const window = new Window();
  window.document.body.innerHTML = renderToStaticMarkup(
    <ThreadTerminalPanel {...panelProps(overrides)} />,
  );
  return window.document;
}

function toolbarActionLabels(toolbar: Element): string[] {
  return Array.from(toolbar.querySelectorAll<HTMLButtonElement>("button[aria-label]"), (button) =>
    button.getAttribute("aria-label"),
  ).filter((label): label is string => label !== null);
}

function viewportProps(overrides: Partial<ViewportProps> = {}): ViewportProps {
  return {
    threadRef: TEST_THREAD_REF,
    threadId: TEST_THREAD_ID,
    projectId: TEST_PROJECT_ID,
    terminalId: "term-1",
    terminalLabel: "Terminal 1",
    cwd: "/repo",
    onSessionExited: () => {},
    onAddTerminalContext: () => {},
    focusRequestId: 0,
    autoFocus: false,
    keybindings: EMPTY_KEYBINDINGS,
    ...overrides,
  };
}

function observedCommand(driverKind: "codex" | "claudeAgent" = "codex"): TerminalLaunchCommand {
  return {
    executable: driverKind === "codex" ? "codex" : "claude",
    args: [],
    activity: {
      driverKind,
      providerInstanceId: ProviderInstanceId.make(`${driverKind}-default`),
    },
  };
}

describe("resolveTerminalSelectionActionPosition", () => {
  const bounds = { left: 100, top: 200, width: 400, height: 300 };

  it("anchors below the selection rect when one is available", () => {
    const position = resolveTerminalSelectionActionPosition({
      bounds,
      selectionRect: { right: 250, bottom: 320 },
      pointer: { x: 999, y: 999 },
      viewport: { width: 1200, height: 900 },
    });
    expect(position).toEqual({ x: 250, y: 324 });
  });

  it("falls back to the panel's top-right corner without a selection or pointer", () => {
    const position = resolveTerminalSelectionActionPosition({
      bounds,
      selectionRect: null,
      pointer: null,
      viewport: { width: 1200, height: 900 },
    });
    expect(position).toEqual({ x: 100 + 400 - 140, y: 200 + 12 });
  });

  it("clamps a pointer position inside the panel bounds", () => {
    const position = resolveTerminalSelectionActionPosition({
      bounds,
      selectionRect: null,
      pointer: { x: 5, y: 5000 },
      viewport: { width: 1200, height: 900 },
    });
    expect(position).toEqual({ x: bounds.left, y: bounds.top + bounds.height });
  });

  it("keeps the action inside the viewport with an 8px margin", () => {
    const position = resolveTerminalSelectionActionPosition({
      bounds,
      selectionRect: { right: 5000, bottom: 5000 },
      pointer: null,
      viewport: { width: 600, height: 500 },
    });
    expect(position).toEqual({ x: 600 - 8, y: 500 - 8 });
  });

  it("never positions above the 8px minimum", () => {
    const position = resolveTerminalSelectionActionPosition({
      bounds: { left: -50, top: -50, width: 10, height: 10 },
      selectionRect: { right: -100, bottom: -100 },
      pointer: null,
      viewport: { width: 300, height: 300 },
    });
    expect(position).toEqual({ x: 8, y: 8 });
  });

  it("derives a fallback viewport from the panel bounds when window is unavailable", () => {
    // In this node test environment `window` is undefined, so the fallback
    // viewport is the panel's bottom-right corner plus an 8px margin.
    const position = resolveTerminalSelectionActionPosition({
      bounds,
      selectionRect: { right: 5000, bottom: 5000 },
      pointer: null,
    });
    expect(position).toEqual({
      x: bounds.left + bounds.width + 8 - 8,
      y: bounds.top + bounds.height + 8 - 8,
    });
  });

  it("reads the viewport from window when one exists and none is passed", () => {
    vi.stubGlobal("window", {
      innerWidth: 320,
      innerHeight: 240,
      addEventListener: () => {},
      removeEventListener: () => {},
    });
    try {
      const position = resolveTerminalSelectionActionPosition({
        bounds,
        selectionRect: { right: 5000, bottom: 5000 },
        pointer: null,
      });
      expect(position).toEqual({ x: 320 - 8, y: 240 - 8 });
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

describe("TerminalViewport", () => {
  it("renders the terminal mount container", () => {
    const markup = renderToStaticMarkup(<TerminalViewport {...viewportProps()} />);
    expect(markup).toContain("overflow-hidden");
    expect(markup).toContain("bg-background");
  });

  it("renders with a worktree path and runtime env", () => {
    const markup = renderToStaticMarkup(
      <TerminalViewport
        {...viewportProps({
          worktreePath: "/repo/worktrees/feature",
          runtimeEnv: { ZED_HINT: "1", PATH_HINT: "2", "": "ignored" },
        })}
      />,
    );
    expect(markup).toContain("<div");
  });

  it("renders with a null worktree path", () => {
    const markup = renderToStaticMarkup(
      <TerminalViewport {...viewportProps({ worktreePath: null })} />,
    );
    expect(markup).toContain("<div");
  });

  it("mounts an eligible activity dock after the xterm mount inside the viewport boundary", () => {
    settingsState.enableTerminalAgentActivity = true;
    const markup = renderToStaticMarkup(
      <TerminalViewport
        {...viewportProps({
          terminalId: "terminal-codex",
          command: observedCommand(),
          visible: true,
        })}
      />,
    );

    const xtermMountIndex = markup.indexOf('data-terminal-xterm-mount="terminal-codex"');
    const activityHostIndex = markup.indexOf(
      'data-provider-terminal-activity-host="terminal-codex"',
    );
    expect(xtermMountIndex).toBeGreaterThanOrEqual(0);
    expect(activityHostIndex).toBeGreaterThan(xtermMountIndex);
    expect(markup.slice(activityHostIndex - 120, activityHostIndex + 120)).toContain(
      "absolute inset-x-0 top-7 bottom-0 z-10",
    );
  });

  it("keeps provider terminal activity off by default without removing the xterm mount", () => {
    const markup = renderToStaticMarkup(
      <TerminalViewport
        {...viewportProps({
          terminalId: "terminal-default-off",
          command: observedCommand(),
          visible: true,
        })}
      />,
    );

    expect(markup).toContain('data-terminal-xterm-mount="terminal-default-off"');
    expect(markup).not.toContain('data-provider-terminal-activity-host="terminal-default-off"');
  });

  it("does not mount an activity host for an ordinary terminal or a hidden pane", () => {
    settingsState.enableTerminalAgentActivity = true;
    const ordinaryMarkup = renderToStaticMarkup(<TerminalViewport {...viewportProps()} />);
    const hiddenProviderMarkup = renderToStaticMarkup(
      <TerminalViewport
        {...viewportProps({
          terminalId: "terminal-hidden",
          command: observedCommand(),
          visible: false,
        })}
      />,
    );

    expect(ordinaryMarkup).not.toContain("data-provider-terminal-activity-host");
    expect(hiddenProviderMarkup).not.toContain("data-provider-terminal-activity-host");
  });
});

describe("ThreadTerminalPanel provider activity isolation", () => {
  it("binds each visible split terminal to its own eligible activity host", () => {
    settingsState.enableTerminalAgentActivity = true;
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["terminal-codex", "terminal-claude"],
          activeTerminalId: "terminal-codex",
          terminalGroups: [
            {
              id: "provider-split",
              terminalIds: ["terminal-codex", "terminal-claude"],
            },
          ],
          activeTerminalGroupId: "provider-split",
          terminalCommandsById: new Map([
            ["terminal-codex", observedCommand("codex")],
            ["terminal-claude", observedCommand("claudeAgent")],
          ]),
        })}
      />,
    );

    expect(markup.match(/data-provider-terminal-activity-host=/g)).toHaveLength(2);
    expect(markup).toContain('data-provider-terminal-activity-host="terminal-codex"');
    expect(markup).toContain('data-provider-terminal-activity-host="terminal-claude"');
  });
});

describe("ThreadTerminalPanel empty state", () => {
  it("renders the empty state as an explicitly owned full-height panel", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({ terminalIds: [], activeTerminalId: "", newShortcutLabel: "Ctrl+T" })}
      />,
    );
    expect(markup).toContain("No terminal sessions for this thread yet.");
    expect(markup).toContain("New Terminal (Ctrl+T)");
    expect(markup).toContain('data-terminal-owner="right-panel"');
    expect(markup).toContain("thread-terminal-panel");
    expect(markup).toContain("h-full");
    expect(markup).not.toContain("cursor-row-resize");
    expect(markup).not.toContain("height:220px");
  });

  it("uses the center owner supplied by a center host", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel {...panelProps({ owner: "center-panel", terminalIds: [] })} />,
    );
    expect(markup).toContain('data-terminal-owner="center-panel"');
    expect(markup).not.toContain("cursor-row-resize");
    expect(markup).not.toContain("height:220px");
    expect(markup).toContain("New Terminal");
  });

  it("treats blank-only terminal ids as an empty terminal list", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel {...panelProps({ terminalIds: ["  ", ""] })} />,
    );
    expect(markup).toContain("No terminal sessions for this thread yet.");
  });
});

describe("ThreadTerminalPanel single terminal", () => {
  it("renders terminal-local actions without a whole-surface close control", () => {
    const markup = renderToStaticMarkup(<ThreadTerminalPanel {...panelProps()} />);
    expect(markup).toContain('aria-label="Split Terminal Horizontally"');
    expect(markup).toContain('aria-label="Split Terminal Vertically"');
    expect(markup).toContain('aria-label="New Terminal"');
    expect(markup).not.toContain('aria-label="Close Terminal"');
    expect(markup).not.toContain("lucide-trash-2");
    expect(markup).not.toContain("Group 1");
  });

  it("does not render a whole-surface close control for a center terminal", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel {...panelProps({ owner: "center-panel" })} />,
    );
    expect(markup).not.toContain('aria-label="Close Terminal"');
    expect(markup).not.toContain("lucide-trash-2");
  });

  it("omits split and new controls when the host does not support them", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          onSplitTerminal: undefined as unknown as () => void,
          onSplitTerminalVertical: undefined as unknown as () => void,
          onNewTerminal: undefined as unknown as () => void,
        })}
      />,
    );

    expect(markup).not.toContain('aria-label="Split Terminal Horizontally"');
    expect(markup).not.toContain('aria-label="Split Terminal Vertically"');
    expect(markup).not.toContain('aria-label="New Terminal"');
    expect(markup).not.toContain("pointer-events-none absolute right-2 top-2 z-20");
  });

  it("includes shortcut labels in the action tooltips when provided", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          splitShortcutLabel: "Ctrl+Shift+H",
          splitVerticalShortcutLabel: "Ctrl+Shift+V",
          newShortcutLabel: "Ctrl+T",
          closeShortcutLabel: "Ctrl+W",
        })}
      />,
    );
    expect(markup).toContain("Split Terminal Horizontally (Ctrl+Shift+H)");
    expect(markup).toContain("Split Terminal Vertically (Ctrl+Shift+V)");
    expect(markup).toContain("New Terminal (Ctrl+T)");
  });

  it("deduplicates and trims terminal ids before rendering", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({ terminalIds: [" term-1 ", "term-1", ""], activeTerminalId: "term-1" })}
      />,
    );
    // A single surviving terminal renders without the multi-terminal sidebar.
    expect(markup).not.toContain("Group 1");
  });
});

describe("ThreadTerminalPanel terminal-local toolbar callback matrix", () => {
  for (const toolbarCase of TERMINAL_TOOLBAR_CASES) {
    it(`renders floating actions without dangling chrome for ${toolbarCase.availability}`, () => {
      const document = renderPanelDocument({
        owner: "center-panel",
        ...toolbarCase.callbacks,
        closeShortcutLabel: "Ctrl+W",
      });
      const toolbar = document.querySelector('[data-terminal-toolbar="floating"]');

      expect(document.querySelector('[data-terminal-toolbar="sidebar"]')).toBeNull();
      expect(document.querySelector('button[aria-label="Close Terminal"]')).toBeNull();
      expect(document.querySelector('button[aria-label="Close Terminal (Ctrl+W)"]')).toBeNull();

      if (toolbarCase.actionLabels.length === 0) {
        expect(toolbar).toBeNull();
        return;
      }

      expect(toolbar).not.toBeNull();
      if (!toolbar) return;
      expect(toolbarActionLabels(toolbar)).toEqual(toolbarCase.actionLabels);

      const sequence = Array.from(toolbar.children).flatMap((child) => {
        if (child.matches('[data-slot="popover-trigger"]')) {
          const label = child.querySelector("button")?.getAttribute("aria-label");
          return label ? [label] : [];
        }
        return child.matches("[data-terminal-toolbar-separator]") ? ["divider"] : [];
      });
      const separatorCount = toolbar.querySelectorAll("[data-terminal-toolbar-separator]").length;

      expect(sequence).toEqual(toolbarCase.floatingSequence);
      expect(separatorCount).toBe(Math.max(toolbarCase.actionLabels.length - 1, 0));
      expect(sequence[0]).not.toBe("divider");
      expect(sequence.at(-1)).not.toBe("divider");
    });

    it(`renders sidebar actions and per-session close controls for ${toolbarCase.availability}`, () => {
      const document = renderPanelDocument({
        owner: "right-panel",
        terminalIds: ["term-1", "term-2"],
        activeTerminalId: "term-1",
        terminalGroups: [{ id: "group-a", terminalIds: ["term-1", "term-2"] }],
        activeTerminalGroupId: "group-a",
        ...toolbarCase.callbacks,
        closeShortcutLabel: "Ctrl+W",
      });
      const toolbar = document.querySelector('[data-terminal-toolbar="sidebar"]');
      const sessionCloseLabels = Array.from(
        document.querySelectorAll<HTMLButtonElement>('button[aria-label^="Close Terminal"]'),
        (button) => button.getAttribute("aria-label"),
      );

      expect(document.querySelector('[data-terminal-toolbar="floating"]')).toBeNull();
      expect(document.querySelector('button[aria-label="Close Terminal"]')).toBeNull();
      expect(document.querySelector('button[aria-label="Close Terminal (Ctrl+W)"]')).toBeNull();
      expect(sessionCloseLabels).toEqual(["Close Terminal 1 (Ctrl+W)", "Close Terminal 2"]);

      if (toolbarCase.actionLabels.length === 0) {
        expect(toolbar).toBeNull();
        return;
      }

      expect(toolbar).not.toBeNull();
      if (!toolbar) return;
      expect(toolbarActionLabels(toolbar)).toEqual(toolbarCase.actionLabels);

      const actionButtons = Array.from(
        toolbar.querySelectorAll<HTMLButtonElement>("button[aria-label]"),
      );
      const dividerBefore = actionButtons.map(
        (button) =>
          button.classList.contains("border-l") && button.classList.contains("border-border/70"),
      );
      const dividerChrome = Array.from(toolbar.querySelectorAll("[class]")).filter(
        (element) =>
          element.classList.contains("border-l") && element.classList.contains("border-border/70"),
      );

      expect(dividerBefore).toEqual(toolbarCase.sidebarDividerBefore);
      expect(dividerChrome).toHaveLength(Math.max(toolbarCase.actionLabels.length - 1, 0));
      expect(dividerBefore[0]).toBe(false);
      expect(dividerBefore.at(-1)).toBe(toolbarCase.actionLabels.length > 1);
    });
  }
});

describe("ThreadTerminalPanel split groups", () => {
  const twoInOneGroup = {
    terminalIds: ["term-1", "term-2"],
    activeTerminalId: "term-1",
    terminalGroups: [{ id: "group-a", terminalIds: ["term-1", "term-2"] }],
    activeTerminalGroupId: "group-a",
  } satisfies Partial<PanelProps>;

  it("renders a horizontal split grid for a two-terminal group", () => {
    const markup = renderToStaticMarkup(<ThreadTerminalPanel {...panelProps(twoInOneGroup)} />);
    expect(markup).toContain("grid-template-columns:repeat(2, minmax(0, 1fr))");
    expect(markup).not.toContain("grid-template-rows");
  });

  it("renders a vertical split grid when the group direction is vertical", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          ...twoInOneGroup,
          terminalGroups: [
            { id: "group-a", terminalIds: ["term-1", "term-2"], splitDirection: "vertical" },
          ],
        })}
      />,
    );
    expect(markup).toContain("grid-template-rows:repeat(2, minmax(0, 1fr))");
  });

  it("disables split actions at the per-group terminal limit", () => {
    const ids = ["term-1", "term-2", "term-3", "term-4"];
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ids,
          activeTerminalId: "term-1",
          terminalGroups: [{ id: "group-a", terminalIds: ids }],
          activeTerminalGroupId: "group-a",
        })}
      />,
    );
    expect(markup).toContain("Split Terminal Horizontally (max 4 per group)");
    expect(markup).toContain("Split Terminal Vertically (max 4 per group)");
    expect(markup).toContain("cursor-not-allowed");
  });

  it("shows group headers when multiple groups exist", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2"],
          activeTerminalId: "term-2",
          terminalGroups: [
            { id: "group-a", terminalIds: ["term-1"] },
            { id: "group-b", terminalIds: ["term-2"] },
          ],
          activeTerminalGroupId: "group-b",
        })}
      />,
    );
    expect(markup).toContain("Group 1");
    expect(markup).toContain("Group 2");
    expect(markup).toContain("Terminal 1");
    expect(markup).toContain("Terminal 2");
  });

  it("sanitizes group definitions: blanks, duplicates, unknown and reassigned ids", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2", "term-3"],
          activeTerminalId: "term-2",
          terminalGroups: [
            // Group with only unknown/blank terminals is dropped entirely.
            { id: "ghost", terminalIds: ["missing", " ", ""] },
            // Blank group id gets a generated one; duplicate ids inside the
            // group collapse to a single entry.
            { id: "  ", terminalIds: ["term-1", "term-1"] },
            // A terminal already assigned above cannot be claimed again.
            { id: "claimed", terminalIds: ["term-1", "term-2"] },
          ],
          activeTerminalGroupId: "claimed",
        })}
      />,
    );
    // term-3 is unassigned and gets its own trailing group: three headers.
    expect(markup).toContain("Group 1");
    expect(markup).toContain("Group 2");
    expect(markup).toContain("Group 3");
    expect(markup).toContain("Terminal 3");
  });

  it("assigns unique group ids when duplicate group ids collide", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2"],
          activeTerminalId: "term-1",
          terminalGroups: [
            { id: "dup", terminalIds: ["term-1"] },
            { id: "dup", terminalIds: ["term-2"] },
          ],
          activeTerminalGroupId: "dup",
        })}
      />,
    );
    expect(markup).toContain("Group 1");
    expect(markup).toContain("Group 2");
  });

  it("keeps probing suffixes when the deduplicated group id is also taken", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2", "term-3"],
          activeTerminalId: "term-1",
          terminalGroups: [
            { id: "dup", terminalIds: ["term-1"] },
            { id: "dup-2", terminalIds: ["term-2"] },
            // Collides with "dup", then with the existing "dup-2" → "dup-3".
            { id: "dup", terminalIds: ["term-3"] },
          ],
          activeTerminalGroupId: "dup",
        })}
      />,
    );
    expect(markup).toContain("Group 1");
    expect(markup).toContain("Group 2");
    expect(markup).toContain("Group 3");
  });

  it("falls back to the first terminal when the active id is unknown", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2"],
          activeTerminalId: "missing",
          terminalGroups: [
            { id: "group-a", terminalIds: ["term-1"] },
            { id: "group-b", terminalIds: ["term-2"] },
          ],
          activeTerminalGroupId: "missing-group",
          closeShortcutLabel: "Ctrl+W",
        })}
      />,
    );
    // The resolved active terminal (term-1) carries the close shortcut label.
    expect(markup).not.toContain('aria-label="Close Terminal"');
    expect(markup).toContain("Close Terminal 1 (Ctrl+W)");
    expect(markup).toContain("Close Terminal 2");
    expect(markup).not.toContain("Close Terminal 2 (Ctrl+W)");
  });
});

describe("ThreadTerminalPanel sidebar labels", () => {
  it("prefers server-provided labels over derived terminal labels", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "custom-shell"],
          activeTerminalId: "term-1",
          terminalLabelsById: new Map([["custom-shell", "vitest watch"]]),
        })}
      />,
    );
    expect(markup).toContain("Terminal 1");
    expect(markup).toContain("vitest watch");
    expect(markup).toContain('aria-label="Close vitest watch"');
  });

  it("uses per-terminal launch locations when the server knows the session", () => {
    const markup = renderToStaticMarkup(
      <ThreadTerminalPanel
        {...panelProps({
          terminalIds: ["term-1", "term-2"],
          activeTerminalId: "term-2",
          runtimeEnv: { FALLBACK: "1" },
          worktreePath: "/repo/worktrees/default",
          terminalLaunchLocationsById: new Map([
            [
              "term-2",
              {
                cwd: "/repo/worktrees/feature",
                worktreePath: "/repo/worktrees/feature",
                runtimeEnv: { FEATURE: "1" },
              },
            ],
          ]),
        })}
      />,
    );
    // Both terminals render sidebar rows; the active one is highlighted.
    expect(markup).toContain("Terminal 1");
    expect(markup).toContain("Terminal 2");
    expect(markup).toContain("bg-accent");
  });
});
