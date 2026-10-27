// @vitest-environment happy-dom

import { ProviderInstanceId } from "@coddy/contracts";
import { act, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

vi.mock("lucide-react", () => ({
  BotIcon: () => <span data-icon="bot" />,
}));

vi.mock("~/providerSkillPresentation", () => ({
  formatProviderSkillInstallSource: (skill: { id?: string }) => `source:${skill.id ?? "unknown"}`,
}));

vi.mock("~/lib/utils", () => ({
  cn: (...values: ReadonlyArray<unknown>) => values.filter(Boolean).join(" "),
}));

vi.mock("./PierreEntryIcon", () => ({
  PierreEntryIcon: (props: { pathValue: string; kind: string; theme: string }) => (
    <span
      data-icon="path"
      data-path={props.pathValue}
      data-kind={props.kind}
      data-theme={props.theme}
    />
  ),
}));

vi.mock("../ui/command", () => ({
  Command: ({
    children,
    onItemHighlighted,
  }: {
    children?: ReactNode;
    onItemHighlighted?: (value: unknown) => void;
  }) => (
    <div data-command>
      <button
        type="button"
        data-highlight-string
        onClick={() => onItemHighlighted?.("item-from-command")}
      />
      <button type="button" data-highlight-null onClick={() => onItemHighlighted?.({})} />
      {children}
    </div>
  ),
  CommandList: ({ children, ...props }: React.ComponentPropsWithoutRef<"div">) => (
    <div data-command-list {...props}>
      {children}
    </div>
  ),
  CommandGroup: ({ children }: { children?: ReactNode }) => (
    <section data-command-group>{children}</section>
  ),
  CommandGroupLabel: ({ children, ...props }: React.ComponentPropsWithoutRef<"h3">) => (
    <h3 data-command-group-label {...props}>
      {children}
    </h3>
  ),
  CommandSeparator: () => <hr data-command-separator />,
  CommandItem: ({
    children,
    value: _value,
    ...props
  }: React.ComponentPropsWithoutRef<"button"> & { value?: string }) => (
    <button type="button" {...props}>
      {children}
    </button>
  ),
}));

import { ComposerCommandMenu } from "./ComposerCommandMenu";
import type { ComposerCommandItem } from "./composerCommandItems";

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
const onHighlightedItemChange = vi.fn();
const onSelect = vi.fn();

function fileItem(id = "file-1"): ComposerCommandItem {
  return {
    id,
    type: "file-reference",
    group: "files",
    path: "src/main.ts",
    pathKind: "file",
    label: "src/main.ts",
    description: "Project file",
    replacement: "@src/main.ts ",
  };
}

function actionItem(id = "action-1"): ComposerCommandItem {
  return {
    id,
    type: "coddy-studio-action",
    group: "coddy-studio",
    action: "plan",
    label: ":plan",
    description: "Switch to plan mode",
    replacement: null,
  };
}

function providerCommandItem(id = "command-1"): ComposerCommandItem {
  return {
    id,
    type: "provider-command",
    group: "commands",
    providerInstanceId: ProviderInstanceId.make("codex"),
    command: { name: "compact" } as never,
    label: "/compact",
    description: "Compact context",
    replacement: "/compact ",
  };
}

function agentItem(id = "agent-1"): ComposerCommandItem {
  return {
    id,
    type: "agent-reference",
    group: "agents",
    providerInstanceId: ProviderInstanceId.make("claude"),
    agent: { name: "planner" } as never,
    label: "@planner",
    description: "Planning agent",
    replacement: "@planner ",
  };
}

function skillItem(id = "skill-1"): ComposerCommandItem {
  return {
    id,
    type: "provider-skill",
    group: "skills",
    providerInstanceId: ProviderInstanceId.make("codex"),
    skill: { path: "/skills/coverage", name: "coverage" } as never,
    label: "$coverage",
    description: "Improve tests",
    replacement: "$coverage ",
  };
}

function renderMenu(
  overrides: Partial<React.ComponentProps<typeof ComposerCommandMenu>> = {},
): ReactElement {
  return (
    <ComposerCommandMenu
      items={[]}
      resolvedTheme="light"
      isLoading={false}
      emptyStateText="No matching command."
      activeItemId={null}
      onHighlightedItemChange={onHighlightedItemChange}
      onSelect={onSelect}
      {...overrides}
    />
  );
}

async function mount(element: ReactElement): Promise<MountedTree> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const mounted = { container, root };
  mountedTrees.push(mounted);
  await act(async () => root.render(element));
  return mounted;
}

async function dispatch(element: HTMLElement, event: Event): Promise<void> {
  await act(async () => element.dispatchEvent(event));
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  onHighlightedItemChange.mockReset();
  onSelect.mockReset();
  vi.stubGlobal("CSS", { escape: (value: string) => value.replaceAll(":", "\\:") });
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("ComposerCommandMenu empty states", () => {
  it("renders the semantic empty-state text", async () => {
    const mounted = await mount(renderMenu({ emptyStateText: "No matching files or agents." }));

    expect(mounted.container.textContent).toContain("No matching files or agents.");
  });

  it("renders a path-search loading state", async () => {
    const mounted = await mount(renderMenu({ isLoading: true }));

    expect(mounted.container.textContent).toContain("Searching workspace files...");
    expect(
      mounted.container
        .querySelector('[data-composer-menu="true"]')
        ?.getAttribute("data-composer-menu-loading"),
    ).toBe("true");
  });

  it("keeps loaded items mounted and marks the menu busy while a refreshed search is pending", async () => {
    const mounted = await mount(renderMenu({ items: [fileItem()], isLoading: true }));
    const menu = mounted.container.querySelector('[data-composer-menu="true"]');
    const item = mounted.container.querySelector('[data-composer-item-id="file-1"]');

    expect(menu?.getAttribute("data-composer-menu-loading")).toBe("true");
    expect(menu?.getAttribute("aria-busy")).toBe("true");
    expect(item).not.toBeNull();
    expect(mounted.container.textContent).toContain("Searching workspace files...");

    await act(async () =>
      mounted.root.render(renderMenu({ items: [fileItem()], isLoading: false })),
    );

    expect(menu?.getAttribute("data-composer-menu-loading")).toBe("false");
    expect(menu?.getAttribute("aria-busy")).toBe("false");
    expect(mounted.container.querySelector('[data-composer-item-id="file-1"]')).toBe(item);
    expect(mounted.container.textContent).not.toContain("Searching workspace files...");
  });
});

describe("ComposerCommandMenu grouping", () => {
  it("renders semantic groups in their fixed order", async () => {
    const mounted = await mount(
      renderMenu({
        items: [agentItem(), fileItem(), skillItem(), providerCommandItem(), actionItem()],
      }),
    );

    expect(
      Array.from(mounted.container.querySelectorAll("[data-command-group-label]"), (node) =>
        node.textContent?.trim(),
      ),
    ).toEqual(["Coddy Studio", "Commands", "Skills", "Files", "Agents"]);
    expect(mounted.container.querySelectorAll("[data-command-separator]")).toHaveLength(4);
  });

  it("labels a single semantic group and shows provider skill source", async () => {
    const mounted = await mount(renderMenu({ items: [skillItem()] }));

    expect(mounted.container.querySelector("[data-command-group-label]")?.textContent).toBe(
      "Skills",
    );
    expect(mounted.container.textContent).toContain("source:unknown");
  });

  it("exposes stable selectors for each semantic group and label", async () => {
    const mounted = await mount(
      renderMenu({
        items: [agentItem(), fileItem(), skillItem(), providerCommandItem(), actionItem()],
      }),
    );

    for (const [id, label] of [
      ["coddy-studio", "Coddy Studio"],
      ["commands", "Commands"],
      ["skills", "Skills"],
      ["files", "Files"],
      ["agents", "Agents"],
    ] as const) {
      const group = mounted.container.querySelector(`[data-composer-group="${id}"]`);
      expect(group).not.toBeNull();
      expect(group?.querySelector(`[data-composer-group-label="${id}"]`)?.textContent?.trim()).toBe(
        label,
      );
    }
  });
});

describe("ComposerCommandMenu item behavior", () => {
  it("renders each item type with its semantic glyph", async () => {
    const mounted = await mount(
      renderMenu({
        items: [fileItem(), actionItem(), providerCommandItem(), agentItem(), skillItem()],
      }),
    );

    expect(mounted.container.querySelector("[data-icon=path]")).toMatchObject({
      dataset: { path: "src/main.ts", kind: "file", theme: "light" },
    });
    expect(mounted.container.querySelectorAll("[data-icon=bot]")).toHaveLength(2);
    expect(mounted.container.querySelectorAll("svg")).toHaveLength(2);
  });

  it("highlights inactive items, preserves active items, and selects by click", async () => {
    const inactive = fileItem("inactive");
    const active = actionItem("active");
    const mounted = await mount(renderMenu({ items: [inactive, active], activeItemId: "active" }));
    const inactiveButton = mounted.container.querySelector<HTMLElement>(
      '[data-composer-item-id="inactive"]',
    )!;
    const activeButton = mounted.container.querySelector<HTMLElement>(
      '[data-composer-item-id="active"]',
    )!;

    expect(inactiveButton.dataset.composerItemActive).toBe("false");
    expect(activeButton.dataset.composerItemActive).toBe("true");
    await dispatch(inactiveButton, new MouseEvent("mousemove", { bubbles: true }));
    await dispatch(activeButton, new MouseEvent("mousemove", { bubbles: true }));
    const mouseDown = new MouseEvent("mousedown", { bubbles: true, cancelable: true });
    await dispatch(inactiveButton, mouseDown);
    await dispatch(inactiveButton, new MouseEvent("click", { bubbles: true }));

    expect(onHighlightedItemChange).toHaveBeenCalledOnce();
    expect(onHighlightedItemChange).toHaveBeenCalledWith("inactive");
    expect(mouseDown.defaultPrevented).toBe(true);
    expect(onSelect).toHaveBeenCalledWith(inactive);
  });

  it("normalizes command-level highlight values", async () => {
    const mounted = await mount(renderMenu());

    await dispatch(
      mounted.container.querySelector<HTMLElement>("[data-highlight-string]")!,
      new MouseEvent("click", { bubbles: true }),
    );
    await dispatch(
      mounted.container.querySelector<HTMLElement>("[data-highlight-null]")!,
      new MouseEvent("click", { bubbles: true }),
    );

    expect(onHighlightedItemChange.mock.calls).toEqual([["item-from-command"], [null]]);
  });

  it("scrolls the active item into view when present", async () => {
    const scrollIntoView = vi
      .spyOn(HTMLElement.prototype, "scrollIntoView")
      .mockImplementation(() => undefined);

    await mount(renderMenu({ items: [fileItem("path:active")], activeItemId: "path:active" }));

    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest" });
  });
});

it("shows skill loading and retryable failures without claiming an empty catalog", async () => {
  const retry = vi.fn();
  const mounted = await mount(
    renderMenu({ isLoading: true, loadingText: "Loading provider skills..." }),
  );
  expect(mounted.container.textContent).toContain("Loading provider skills...");
  await act(async () =>
    mounted.root.render(
      renderMenu({ issueText: "Could not load provider skills.", onRetry: retry }),
    ),
  );
  expect(mounted.container.textContent).toContain("Could not load provider skills.");
  expect(mounted.container.textContent).not.toContain("No matching command.");
  const button = Array.from(mounted.container.querySelectorAll("button")).find(
    (button) => button.textContent === "Retry",
  );
  expect(button).toBeDefined();
  await dispatch(button!, new MouseEvent("click", { bubbles: true }));
  expect(retry).toHaveBeenCalledOnce();
});
