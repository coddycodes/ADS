// @vitest-environment happy-dom

import { act, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  rowActions: [] as Array<Record<string, unknown>>,
  contextGroups: [] as Array<Array<Record<string, unknown>>>,
  buttons: [] as Array<Record<string, unknown>>,
  checkboxes: [] as Array<Record<string, unknown>>,
  menus: [] as Array<Record<string, unknown>>,
  popups: [] as Array<Record<string, unknown>>,
  menuItems: [] as Array<Record<string, unknown>>,
  separators: 0,
}));

vi.mock("./SourceControlRowActions.logic", () => ({
  buildRowContextMenu: () => ({ groups: harness.contextGroups }),
  getRowActions: () => harness.rowActions,
  rowAreaOf: (area: unknown) => area ?? "unstaged",
}));
vi.mock("~/components/chat/DiffStatLabel", () => ({
  DiffStatLabel: (props: Record<string, unknown>) => (
    <span>{`${props.additions as number}/${props.deletions as number}`}</span>
  ),
}));
vi.mock("~/components/ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    harness.buttons.push(props);
    return <button type="button">{props.children as ReactNode}</button>;
  },
}));
vi.mock("~/components/ui/checkbox", () => ({
  Checkbox: (props: Record<string, unknown>) => {
    harness.checkboxes.push(props);
    return <input type="checkbox" aria-label={props["aria-label"] as string} />;
  },
}));
vi.mock("~/components/ui/menu", () => ({
  Menu: (props: Record<string, unknown>) => {
    harness.menus.push(props);
    return <div>{props.children as ReactNode}</div>;
  },
  MenuPopup: (props: Record<string, unknown>) => {
    harness.popups.push(props);
    return <div>{props.children as ReactNode}</div>;
  },
  MenuItem: (props: Record<string, unknown>) => {
    harness.menuItems.push(props);
    return <button type="button">{props.children as ReactNode}</button>;
  },
  MenuSeparator: () => {
    harness.separators += 1;
    return <hr />;
  },
}));

import {
  SourceControlChangesList,
  type SourceControlListSection,
} from "./SourceControlChangesList";

const Icon = () => <span>icon</span>;
const nestedFile = {
  path: "src/file.ts",
  insertions: 3,
  deletions: 1,
  status: "modified" as const,
  area: "unstaged" as const,
};
const rootFile = {
  path: "README.md",
  insertions: 1,
  deletions: 0,
  status: "untracked" as const,
  area: "untracked" as const,
};

let host: HTMLDivElement;
let root: Root;

function clearCaptures(): void {
  harness.buttons.length = 0;
  harness.checkboxes.length = 0;
  harness.menus.length = 0;
  harness.popups.length = 0;
  harness.menuItems.length = 0;
  harness.separators = 0;
}

function renderList(
  overrides: Partial<SourceControlListSection> = {},
  options: { groupByFolder?: boolean; sections?: SourceControlListSection[] } = {},
) {
  const section: SourceControlListSection = {
    title: "Changes",
    files: [nestedFile, rootFile],
    onToggle: vi.fn(),
    onOpenFile: vi.fn(),
    ...overrides,
  };
  clearCaptures();
  act(() =>
    root.render(
      <SourceControlChangesList
        sections={options.sections ?? [section]}
        {...(options.groupByFolder === undefined ? {} : { groupByFolder: options.groupByFolder })}
      />,
    ),
  );
  return section;
}

/** Runs a captured handler as a user event would, then re-captures the render. */
function trigger(props: Record<string, unknown> | undefined, key: string, ...args: unknown[]) {
  const handler = props?.[key];
  if (typeof handler !== "function") throw new Error(`Missing ${key} handler`);
  act(() => {
    clearCaptures();
    handler(...args);
  });
}

function rowOf(path: string): HTMLElement {
  const open = host.querySelector<HTMLButtonElement>(`button[title="${path}"]`);
  const row = open?.parentElement;
  if (!row) throw new Error(`Missing row ${path}`);
  return row;
}

function rightClick(path: string, x: number, y: number): MouseEvent {
  const event = new MouseEvent("contextmenu", {
    bubbles: true,
    cancelable: true,
    clientX: x,
    clientY: y,
  });
  act(() => {
    clearCaptures();
    rowOf(path).dispatchEvent(event);
  });
  return event;
}

function folderHeaderButtons(): HTMLButtonElement[] {
  return [...host.querySelectorAll<HTMLButtonElement>("button[aria-expanded]")].filter((button) =>
    button.hasAttribute("aria-label"),
  );
}

function click(element: HTMLElement | undefined): void {
  if (!element) throw new Error("Missing element");
  act(() => {
    clearCaptures();
    element.click();
  });
}

function checkboxByLabel(label: string): Record<string, unknown> | undefined {
  return harness.checkboxes.find((props) => props["aria-label"] === label);
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  harness.rowActions = [];
  harness.contextGroups = [];
  clearCaptures();
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

describe("SourceControlChangesList", () => {
  it("renders its empty state", () => {
    renderList({ files: [] });
    expect(host.textContent).toContain("No changes");
  });

  it("renders rows, opens files, and opens the menu at the click point", () => {
    harness.contextGroups = [[{ id: "view", label: "View", enabled: true }]];
    const section = renderList();
    expect(host.textContent).toContain("src");
    expect(host.textContent).toContain("README.md");
    expect(host.textContent).toContain("3/1");
    // Every row carries its status letter.
    expect(host.querySelector('[title="Modified"]')?.textContent).toBe("M");

    click(host.querySelector<HTMLButtonElement>('button[title="src/file.ts"]') ?? undefined);
    expect(section.onOpenFile).toHaveBeenCalledWith("src/file.ts", "unstaged");

    const event = rightClick("src/file.ts", 12, 34);
    expect(event.defaultPrevented).toBe(true);
    const anchor = harness.popups[0]?.anchor as { getBoundingClientRect: () => DOMRect };
    expect(anchor.getBoundingClientRect()).toMatchObject({ x: 12, y: 34, width: 0, height: 0 });
  });

  it("renders one staging checkbox per row in normal mode", () => {
    renderList({ checked: () => false, selected: () => false, onSelect: vi.fn() });

    // Root files group first, and every folder header owns the checkbox above
    // the rows it covers.
    expect(harness.checkboxes.map((props) => props["aria-label"])).toEqual([
      "Stage all files in the repository root",
      "Stage README.md",
      "Stage all files in src",
      "Stage src/file.ts",
    ]);
  });

  it("replaces staging with selection and hides row mutations in selection mode", () => {
    harness.rowActions = [{ kind: "stage", label: "Stage", destructive: false, icon: Icon }];
    const section = renderList({
      selectionMode: true,
      checked: () => false,
      selected: (file) => file.path === nestedFile.path,
      onSelect: vi.fn(),
      onStageFile: vi.fn(),
    });

    expect(harness.checkboxes.map((props) => props["aria-label"])).toEqual([
      "Select all files in the repository root",
      "Select README.md",
      "Deselect all files in src",
      "Deselect src/file.ts",
    ]);
    expect(harness.buttons).toHaveLength(0);
    expect(host.innerHTML).toContain("bg-accent/70");
    trigger(harness.checkboxes[1], "onCheckedChange", true);
    expect(section.onSelect).toHaveBeenCalledWith("README.md", true);
    expect(section.onToggle).not.toHaveBeenCalled();
  });

  it("renders inline row actions with the file in their labels", () => {
    harness.rowActions = [{ kind: "stage", label: "Stage", destructive: false, icon: Icon }];
    const section = renderList({ onStageFile: vi.fn() });

    const stage = harness.buttons.find((props) => props["aria-label"] === "Stage src/file.ts");
    trigger(stage, "onClick", { stopPropagation: vi.fn() });
    expect(section.onStageFile).toHaveBeenCalledWith("src/file.ts");
  });

  it("keeps the right-click menu mutation-free in selection mode", () => {
    harness.contextGroups = [
      [{ id: "view", label: "View", enabled: true }],
      [
        { id: "ignore-file-name", label: "Ignore File", enabled: true },
        { id: "ignore-parent-folder", label: "Ignore Parent", enabled: true },
      ],
    ];
    const section = renderList({
      selectionMode: true,
      onIgnoreFileName: vi.fn(),
      onIgnoreParentFolder: vi.fn(),
    });

    const event = rightClick("src/file.ts", 20, 30);
    expect(event.defaultPrevented).toBe(true);
    expect(harness.menuItems.map((item) => item.children)).toEqual(["View"]);
    expect(section.onIgnoreFileName).not.toHaveBeenCalled();
    expect(section.onIgnoreParentFolder).not.toHaveBeenCalled();
  });

  it("executes every available context-menu action and closes the menu", () => {
    harness.contextGroups = [
      [
        { id: "view", label: "View", enabled: true },
        { id: "copy-path", label: "Copy Path", enabled: true },
        { id: "copy-relative-path", label: "Copy Relative", enabled: true },
      ],
      [
        { id: "ignore-file-name", label: "Ignore File", enabled: true },
        { id: "ignore-parent-folder", label: "Ignore Parent", enabled: false },
        { id: "open-external-editor", label: "Open External", enabled: true },
      ],
    ];
    const section = renderList({
      isPrimaryEnv: true,
      onViewFile: vi.fn(),
      onCopyPath: vi.fn(),
      onIgnoreFileName: vi.fn(),
      onIgnoreParentFolder: vi.fn(),
      onOpenExternalEditor: vi.fn(),
    });
    rightClick("src/file.ts", 20, 30);
    expect(harness.menuItems).toHaveLength(6);
    expect(harness.separators).toBe(1);
    const items = [...harness.menuItems];
    const popup = harness.popups[0];
    for (const item of items) {
      const onClick = item.onClick;
      if (typeof onClick !== "function") throw new Error("Missing menu item handler");
      act(() => onClick());
    }
    expect(section.onViewFile).toHaveBeenCalledWith("src/file.ts", "unstaged");
    expect(section.onCopyPath).toHaveBeenNthCalledWith(1, "src/file.ts", false);
    expect(section.onCopyPath).toHaveBeenNthCalledWith(2, "src/file.ts", true);
    expect(section.onIgnoreFileName).toHaveBeenCalledWith("src/file.ts");
    expect(section.onIgnoreParentFolder).toHaveBeenCalledWith("src/file.ts");
    expect(section.onOpenExternalEditor).toHaveBeenCalledWith("src/file.ts");

    const stopPropagation = vi.fn();
    const preventDefault = vi.fn();
    if (!popup) throw new Error("Missing menu popup");
    (popup.onClick as (event: unknown) => void)({ stopPropagation });
    (popup.onContextMenu as (event: unknown) => void)({ preventDefault });
    expect(stopPropagation).toHaveBeenCalledOnce();
    expect(preventDefault).toHaveBeenCalledOnce();

    // Choosing an item closed the menu.
    clearCaptures();
    act(() => root.render(<SourceControlChangesList sections={[section]} />));
    expect(harness.menus).toHaveLength(0);

    // Dismissing the menu closes it too; reopening it does not.
    rightClick("src/file.ts", 20, 30);
    const reopened = harness.menus[0];
    trigger(reopened, "onOpenChange", true);
    expect(host.textContent).toContain("Copy Path");
    trigger(reopened, "onOpenChange", false);
    expect(host.textContent).not.toContain("Copy Path");
  });

  it("drops context-menu groups whose handlers are unavailable", () => {
    harness.contextGroups = [
      [
        { id: "copy-path", label: "Copy", enabled: true },
        { id: "copy-relative-path", label: "Copy Relative", enabled: true },
        { id: "ignore-file-name", label: "Ignore", enabled: true },
        { id: "ignore-parent-folder", label: "Ignore Parent", enabled: true },
        { id: "open-external-editor", label: "Open", enabled: true },
      ],
    ];
    renderList();
    rightClick("README.md", 0, 0);
    expect(harness.menus).toHaveLength(0);
  });

  it("groups rows under a collapsible folder header per directory", () => {
    renderList();
    const headers = folderHeaderButtons();

    expect(headers.map((header) => header.title)).toEqual(["Repository root", "src"]);
    expect(headers.map((header) => header.getAttribute("aria-expanded"))).toEqual(["true", "true"]);
    // The label is start-truncated and the count is a bare numeral, so the
    // button names the folder and its size itself.
    expect(headers.map((header) => header.getAttribute("aria-label"))).toEqual([
      "Repository root, 1 file",
      "src, 1 file",
    ]);
    // Both rows stay visible while every folder is expanded.
    expect(host.textContent).toContain("README.md");
    expect(host.textContent).toContain("file.ts");
  });

  it("toggles one folder at a time and hides the rows of a collapsed folder", () => {
    renderList();

    click(folderHeaderButtons()[1]);
    expect(host.textContent).not.toContain("file.ts");
    expect(host.textContent).toContain("README.md");
    expect(folderHeaderButtons()[1]?.getAttribute("aria-expanded")).toBe("false");

    click(folderHeaderButtons()[1]);
    expect(host.textContent).toContain("file.ts");
  });

  it("keeps folder collapse per section", () => {
    const staged = { ...nestedFile, area: "staged" as const };
    renderList(
      {},
      {
        sections: [
          { title: "Staged Changes", files: [staged], onToggle: vi.fn(), onOpenFile: vi.fn() },
          { title: "Changes", files: [nestedFile], onToggle: vi.fn(), onOpenFile: vi.fn() },
        ],
      },
    );
    expect(host.querySelectorAll('button[title="src/file.ts"]')).toHaveLength(2);

    click(folderHeaderButtons()[0]);
    expect(host.querySelectorAll('button[title="src/file.ts"]')).toHaveLength(1);
  });

  it("collapses a whole section from its header", () => {
    renderList();
    const sectionHeader = [...host.querySelectorAll<HTMLButtonElement>("button")].find((button) =>
      button.textContent?.startsWith("Changes"),
    );
    expect(sectionHeader?.getAttribute("aria-expanded")).toBe("true");

    click(sectionHeader);
    expect(host.textContent).not.toContain("README.md");
    expect(folderHeaderButtons()).toHaveLength(0);
    expect(host.querySelector("button")?.getAttribute("aria-expanded")).toBe("false");
  });

  it("keeps a collapsed folder collapsed across its section collapsing", () => {
    renderList();
    click(folderHeaderButtons()[1]);
    const sectionHeader = () => host.querySelector<HTMLButtonElement>("button") ?? undefined;
    click(sectionHeader());
    click(sectionHeader());

    expect(folderHeaderButtons()[1]?.getAttribute("aria-expanded")).toBe("false");
    expect(host.textContent).not.toContain("file.ts");
  });

  it("stages only the not-yet-staged files of a mixed folder in one request", () => {
    const section = renderList({
      files: [nestedFile, { ...nestedFile, path: "src/other.ts" }],
      checked: (file) => file.path === "src/file.ts",
      onStageFiles: vi.fn(),
      onUnstageFiles: vi.fn(),
    });

    const header = checkboxByLabel("Stage all files in src");
    expect(header?.indeterminate).toBe(true);
    expect(header?.checked).toBe(false);
    trigger(header, "onCheckedChange", true);
    expect(section.onStageFiles).toHaveBeenCalledWith(["src/other.ts"]);
    expect(section.onUnstageFiles).not.toHaveBeenCalled();
    expect(section.onToggle).not.toHaveBeenCalled();
  });

  it("unstages a fully staged folder in one request", () => {
    const section = renderList({
      files: [nestedFile, { ...nestedFile, path: "src/other.ts" }],
      checked: () => true,
      onStageFiles: vi.fn(),
      onUnstageFiles: vi.fn(),
    });

    const header = checkboxByLabel("Unstage all files in src");
    expect(header?.checked).toBe(true);
    expect(header?.indeterminate).toBe(false);
    trigger(header, "onCheckedChange", false);
    expect(section.onUnstageFiles).toHaveBeenCalledWith(["src/file.ts", "src/other.ts"]);
  });

  it("falls back to per-file toggling when no batch handler is wired", () => {
    const section = renderList({
      files: [nestedFile, { ...nestedFile, path: "src/other.ts" }],
      checked: () => false,
    });

    trigger(checkboxByLabel("Stage all files in src"), "onCheckedChange", true);
    expect(vi.mocked(section.onToggle).mock.calls.map((call) => call[0])).toEqual([
      "src/file.ts",
      "src/other.ts",
    ]);
  });

  it("selects a whole folder through the batch selection handler", () => {
    const section = renderList({
      selectionMode: true,
      selected: () => false,
      onSelect: vi.fn(),
      onSelectFiles: vi.fn(),
    });

    trigger(checkboxByLabel("Select all files in src"), "onCheckedChange", true);
    expect(section.onSelectFiles).toHaveBeenCalledWith([nestedFile], true);
    expect(section.onSelect).not.toHaveBeenCalled();
  });

  it("renders a flat list with per-row directories when grouping is off", () => {
    renderList({ checked: () => false }, { groupByFolder: false });

    expect(folderHeaderButtons()).toHaveLength(0);
    expect(harness.checkboxes.map((props) => props["aria-label"])).toEqual([
      "Stage src/file.ts",
      "Stage README.md",
    ]);
    // The row keeps its own directory hint, truncated from the start.
    expect(host.innerHTML).toContain('dir="rtl"');
    expect(host.textContent).toContain("src");
  });
});
