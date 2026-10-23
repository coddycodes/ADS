import { EnvironmentId } from "@bibcode/contracts";
import { AsyncResult } from "effect/reactivity";
import type { ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

type Effect = () => void | (() => void);

const hooks = vi.hoisted(() => {
  let cursor = 0;
  let states = new Map<number, unknown>();
  let dependencies = new Map<number, ReadonlyArray<unknown> | undefined>();
  const effects: Effect[] = [];

  const dependenciesChanged = (
    previous: ReadonlyArray<unknown> | undefined,
    next: ReadonlyArray<unknown> | undefined,
  ) =>
    previous === undefined ||
    next === undefined ||
    previous.length !== next.length ||
    previous.some((value, index) => !Object.is(value, next[index]));

  return {
    beginRender() {
      cursor = 0;
      effects.length = 0;
    },
    reset() {
      cursor = 0;
      states = new Map();
      dependencies = new Map();
      effects.length = 0;
    },
    runEffects() {
      for (const effect of effects.splice(0)) effect();
    },
    useState<T>(initial: T | (() => T)) {
      const index = cursor++;
      if (!states.has(index)) {
        states.set(index, typeof initial === "function" ? (initial as () => T)() : initial);
      }
      const setState = (next: T | ((previous: T) => T)) => {
        const previous = states.get(index) as T;
        const value = typeof next === "function" ? (next as (value: T) => T)(previous) : next;
        states.set(index, value);
        // The browser's first state is the picker identity; report the path it browses.
        if (index === 0) harness.setPath((value as { readonly path: string }).path);
      };
      return [states.get(index) as T, setState] as const;
    },
    useEffect(effect: Effect, nextDependencies?: ReadonlyArray<unknown>) {
      const index = cursor++;
      const previous = dependencies.get(index);
      if (dependenciesChanged(previous, nextDependencies)) effects.push(effect);
      dependencies.set(index, nextDependencies);
    },
  };
});

const harness = vi.hoisted(() => ({
  browseInput: null as unknown,
  buttons: [] as Array<Record<string, unknown>>,
  input: null as Record<string, unknown> | null,
  browse: vi.fn((input: unknown) => input),
  refresh: vi.fn(),
  createEntry: vi.fn(),
  setPath: vi.fn(),
  query: {
    data: null as null | Record<string, unknown>,
    error: null as string | null,
    isPending: false,
    refresh: () => {},
    revalidate: () => {},
    requiresRetry: false,
  },
  reset() {
    this.browseInput = null;
    this.buttons.length = 0;
    this.input = null;
    this.browse.mockClear();
    this.refresh.mockReset();
    this.createEntry
      .mockReset()
      .mockResolvedValue(AsyncResult.success({ relativePath: "new-folder" }));
    this.setPath.mockReset();
    this.query = {
      data: null,
      error: null,
      isPending: false,
      refresh: this.refresh,
      revalidate: this.refresh,
      requiresRetry: false,
    };
  },
}));

vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  return {
    ...actual,
    useState: hooks.useState as typeof actual.useState,
    useEffect: hooks.useEffect as typeof actual.useEffect,
  };
});

vi.mock("~/state/filesystem", () => ({
  filesystemEnvironment: {
    browse: (input: unknown) => {
      harness.browseInput = input;
      return harness.browse(input);
    },
  },
}));

vi.mock("~/state/query", () => ({ useEnvironmentQuery: () => harness.query }));

vi.mock("~/state/projects", () => ({
  projectEnvironment: { createEntry: "project-create-entry" },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => harness.createEntry,
}));

vi.mock("../ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    harness.buttons.push(props);
    return (
      <button type="button" disabled={Boolean(props.disabled)} onClick={props.onClick as never}>
        {props.children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../ui/dialog", () => ({
  Dialog: ({ open, children }: { open?: boolean; children?: ReactNode }) =>
    open ? <div>{children}</div> : null,
  DialogPopup: ({ children }: { children?: ReactNode }) => <div>{children}</div>,
  DialogHeader: ({ children }: { children?: ReactNode }) => <header>{children}</header>,
  DialogTitle: ({ children }: { children?: ReactNode }) => <h2>{children}</h2>,
  DialogDescription: ({ children }: { children?: ReactNode }) => <p>{children}</p>,
  DialogFooter: ({ children }: { children?: ReactNode }) => <footer>{children}</footer>,
}));

vi.mock("../ui/draft-input", () => ({
  DraftInput: (props: Record<string, unknown>) => {
    harness.input = props;
    return (
      <input aria-label={props["aria-label"] as string} value={props.value as string} readOnly />
    );
  },
}));

vi.mock("../ui/input", () => ({
  Input: (props: Record<string, unknown>) => (
    <input
      aria-label={props["aria-label"] as string}
      value={props.value as string}
      disabled={Boolean(props.disabled)}
      onChange={props.onChange as never}
      onKeyDown={props.onKeyDown as never}
      readOnly={typeof props.onChange !== "function"}
    />
  ),
}));

const { RemoteDirectoryPickerDialog } = await import("./RemoteDirectoryPickerDialog");
type RemoteDirectoryPickerDialogProps = React.ComponentProps<typeof RemoteDirectoryPickerDialog>;

function renderPicker(props: RemoteDirectoryPickerDialogProps): string {
  harness.browseInput = null;
  harness.buttons.length = 0;
  hooks.beginRender();
  return renderToStaticMarkup(<RemoteDirectoryPickerDialog {...props} />);
}

function button(label: string): Record<string, unknown> {
  const match = harness.buttons.find(
    (props) => props.children === label || props["aria-label"] === label,
  );
  if (!match) throw new Error(`Missing button: ${label}`);
  return match;
}

function invokeClick(props: Record<string, unknown>): void {
  if (typeof props.onClick !== "function") throw new Error("Missing click handler");
  props.onClick();
}

function pickerProps(
  overrides: Partial<RemoteDirectoryPickerDialogProps> = {},
): RemoteDirectoryPickerDialogProps {
  return {
    open: true,
    environmentId: EnvironmentId.make("windows-host"),
    initialPath: "D:\\Worktrees",
    onOpenChange: vi.fn(),
    onSelect: vi.fn(),
    ...overrides,
  };
}

describe("RemoteDirectoryPickerDialog", () => {
  beforeEach(() => {
    hooks.reset();
    harness.reset();
  });

  afterEach(() => vi.restoreAllMocks());

  it("browses the selected server and selects its canonical directory", () => {
    harness.query = {
      data: {
        parentPath: "D:\\Worktrees",
        directoryPath: "D:\\Worktrees",
        ancestorPath: "D:\\",
        breadcrumbs: [
          { name: "D:\\", fullPath: "D:\\" },
          { name: "Worktrees", fullPath: "D:\\Worktrees" },
        ],
        entries: [{ name: "bibcode", fullPath: "D:\\Worktrees\\bibcode" }],
      },
      error: null,
      isPending: false,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    const onSelect = vi.fn();
    renderPicker(pickerProps({ initialPath: "~", onSelect }));

    expect(harness.browseInput).toEqual({
      environmentId: EnvironmentId.make("windows-host"),
      input: { partialPath: "~", mode: "directory" },
    });
    expect(harness.input?.value).toBe("D:\\Worktrees");
    invokeClick(button("Open bibcode"));
    expect(harness.setPath).toHaveBeenCalledWith("D:\\Worktrees\\bibcode");
    invokeClick(button("Select folder"));
    expect(onSelect).toHaveBeenCalledWith("D:\\Worktrees");
  });

  it("navigates with Up and breadcrumb buttons", () => {
    harness.query.data = {
      parentPath: "/srv/projects/app",
      directoryPath: "/srv/projects/app",
      ancestorPath: "/srv/projects",
      breadcrumbs: [
        { name: "srv", fullPath: "/srv" },
        { name: "projects", fullPath: "/srv/projects" },
      ],
      entries: [],
    };
    renderPicker(pickerProps({ initialPath: "/srv/projects/app" }));

    const up = harness.buttons.find((props) => props["aria-label"] === "Up one directory");
    if (!up) throw new Error("Missing Up button");
    invokeClick(up);
    invokeClick(button("Open projects"));
    expect(harness.setPath).toHaveBeenNthCalledWith(1, "/srv/projects");
    expect(harness.setPath).toHaveBeenNthCalledWith(2, "/srv/projects");
  });

  it("refreshes, commits a path, and cancels", () => {
    const onOpenChange = vi.fn();
    renderPicker(pickerProps({ onOpenChange }));

    invokeClick(button("Refresh"));
    const input = harness.input;
    if (!input || typeof input.onCommit !== "function") throw new Error("Missing directory input");
    input.onCommit("/remote/next");
    invokeClick(button("Cancel"));

    expect(harness.refresh).toHaveBeenCalledOnce();
    expect(harness.setPath).toHaveBeenCalledWith("/remote/next");
    expect(onOpenChange).toHaveBeenCalledWith(false);
  });

  it("disables selection while loading and exposes query errors", () => {
    harness.query = {
      data: { parentPath: "/repo", directoryPath: "/repo", entries: [] },
      error: "Permission denied",
      isPending: true,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    const markup = renderPicker(pickerProps({ initialPath: "/repo" }));

    expect(button("Select folder").disabled).toBe(true);
    expect(markup).toContain("Permission denied");
  });

  it("enables New folder only when the query has a canonical current directory", () => {
    expect(renderPicker(pickerProps())).toContain("New folder");
    expect(button("New folder").disabled).toBe(true);

    harness.query = {
      data: { parentPath: "/repo", directoryPath: "/repo", entries: [] },
      error: null,
      isPending: false,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    expect(renderPicker(pickerProps({ initialPath: "/repo" }))).toContain("New folder");
    expect(button("New folder").disabled).toBe(false);

    harness.query.isPending = true;
    renderPicker(pickerProps({ initialPath: "/repo" }));
    expect(button("New folder").disabled).toBe(true);
  });

  it("collapses dot-directories by default and reveals them on request", () => {
    harness.query.data = {
      parentPath: "/home/agent",
      directoryPath: "/home/agent",
      ancestorPath: "/home",
      breadcrumbs: [{ name: "agent", fullPath: "/home/agent" }],
      entries: [
        { name: ".cache", fullPath: "/home/agent/.cache" },
        { name: "Projects", fullPath: "/home/agent/Projects" },
      ],
    };
    const props = pickerProps({ initialPath: "~" });

    const collapsedMarkup = renderPicker(props);
    expect(collapsedMarkup).toContain("Projects");
    expect(collapsedMarkup).not.toContain(".cache");
    expect(collapsedMarkup).toContain("Show hidden folders");

    invokeClick(button("Show hidden folders"));
    const expandedMarkup = renderPicker(props);
    expect(expandedMarkup).toContain(".cache");
    expect(expandedMarkup).toContain("Hide hidden folders");
  });

  it("renders a segmented breadcrumb rail and compact folder rows", () => {
    harness.query.data = {
      parentPath: "C:\\Users\\mauro",
      directoryPath: "C:\\Users\\mauro",
      ancestorPath: "C:\\Users",
      breadcrumbs: [
        { name: "C:\\", fullPath: "C:\\" },
        { name: "Users", fullPath: "C:\\Users" },
        { name: "mauro", fullPath: "C:\\Users\\mauro" },
      ],
      entries: [{ name: "Projects", fullPath: "C:\\Users\\mauro\\Projects" }],
    };

    const markup = renderPicker(pickerProps({ initialPath: "~" }));

    expect(markup).toContain("data-directory-breadcrumb-separator");
    expect(button("Open C:\\")["aria-current"]).toBeUndefined();
    expect(button("Open mauro")["aria-current"]).toBe("page");
    expect(button("Open Projects").className).toContain("justify-start");
    expect(markup).toContain("data-directory-folder-icon");
  });

  it("renders useful loading and visible-empty states", () => {
    harness.query = {
      data: null,
      error: null,
      isPending: true,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    expect(renderPicker(pickerProps())).toContain("Loading folders");

    harness.query = {
      data: {
        parentPath: "/home/agent",
        directoryPath: "/home/agent",
        entries: [{ name: ".cache", fullPath: "/home/agent/.cache" }],
      },
      error: null,
      isPending: false,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    const markup = renderPicker(pickerProps({ initialPath: "~" }));
    expect(markup).toContain("No visible folders");
    expect(markup).toContain("Show hidden folders");
  });

  it("falls back to server home after an inaccessible configured directory", () => {
    harness.query = {
      data: null,
      error: "Directory is unavailable",
      isPending: false,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    const props = pickerProps({ initialPath: "/inaccessible" });
    renderPicker(props);
    hooks.runEffects();
    const fallbackMarkup = renderPicker(props);

    expect(harness.browseInput).toEqual({
      environmentId: EnvironmentId.make("windows-host"),
      input: { partialPath: "~", mode: "directory" },
    });
    expect(fallbackMarkup).toContain("The previous folder is unavailable");
    expect(button("Select folder").disabled).toBe(true);

    harness.query = {
      data: { parentPath: "/home/agent", directoryPath: "/home/agent", entries: [] },
      error: null,
      isPending: false,
      refresh: harness.refresh,
      revalidate: harness.refresh,
      requiresRetry: false,
    };
    renderPicker(props);
    expect(button("Select folder").disabled).toBe(false);
  });

  it("does not read the desktop bridge", () => {
    const desktopBridgeWindow = new Proxy(
      {},
      {
        get(_, key) {
          if (key === "desktopBridge") throw new Error("desktop bridge must not be read");
          return undefined;
        },
      },
    );
    vi.stubGlobal("window", desktopBridgeWindow);

    expect(() => renderPicker(pickerProps())).not.toThrow();
  });
});
