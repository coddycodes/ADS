import React from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

const harness = vi.hoisted(() => ({
  buttons: [] as Array<Record<string, unknown>>,
}));

vi.mock("~/components/ui/button", () => ({
  Button: (props: Record<string, unknown>) => {
    harness.buttons.push(props);
    return <button type="button">{props.children as React.ReactNode}</button>;
  },
}));

import type { SourceControlListSection } from "./SourceControlChangesList";
import { SourceControlSectionHeader } from "./SourceControlSection";

const file = {
  path: "src/file.ts",
  insertions: 3,
  deletions: 1,
  status: "added" as const,
  area: "unstaged" as const,
};

function renderHeader(
  overrides: Partial<SourceControlListSection> = {},
  options: { collapsed?: boolean } = {},
) {
  const element = (
    <SourceControlSectionHeader
      section={{
        title: "Changes",
        files: [file],
        onToggle: vi.fn(),
        onOpenFile: vi.fn(),
        ...overrides,
      }}
      collapsed={options.collapsed ?? false}
      onToggleCollapsed={vi.fn()}
    />
  );
  return { markup: renderToStaticMarkup(element) };
}

function invokeClick(props: Record<string, unknown> | undefined): void {
  if (typeof props?.onClick !== "function") throw new Error("Missing click handler");
  props.onClick();
}

beforeEach(() => {
  harness.buttons.length = 0;
});

describe("SourceControlSectionHeader", () => {
  // Collapsing through this header is covered by the changes list behavior tests.
  it("shows the title, count and expanded state", () => {
    const { markup } = renderHeader();
    expect(markup).toContain("Changes");
    expect(markup).toContain('aria-expanded="true"');
    expect(markup).toContain(">1</span>");
  });

  it("renders stage and discard controls", () => {
    const primary = vi.fn();
    const discard = vi.fn();
    renderHeader({
      disabled: false,
      primaryAction: { icon: "stage", label: "Stage all", onClick: primary },
      onDiscard: discard,
    });
    expect(harness.buttons[0]).toMatchObject({ "aria-label": "Stage all", disabled: false });
    expect(harness.buttons[1]).toMatchObject({
      "aria-label": "Discard all",
      title: "Discard all",
      disabled: false,
    });
    invokeClick(harness.buttons[0]);
    invokeClick(harness.buttons[1]);
    expect(primary).toHaveBeenCalledOnce();
    expect(discard).toHaveBeenCalledOnce();
  });

  it("shows only selection controls while selection mode is active", () => {
    const primary = vi.fn();
    const discard = vi.fn();
    const onSelectAll = vi.fn();

    renderHeader({
      selectionMode: true,
      allSelected: false,
      onSelectAll,
      primaryAction: { icon: "stage", label: "Stage all", onClick: primary },
      onDiscard: discard,
    });

    expect(harness.buttons).toHaveLength(1);
    expect(harness.buttons[0]?.children).toBe("Select all");
    invokeClick(harness.buttons[0]);
    expect(onSelectAll).toHaveBeenCalledWith(true);
    expect(primary).not.toHaveBeenCalled();
    expect(discard).not.toHaveBeenCalled();
  });

  it("clears a fully selected section", () => {
    const onSelectAll = vi.fn();
    renderHeader({ selectionMode: true, allSelected: true, onSelectAll });

    expect(harness.buttons[0]?.children).toBe("Clear section");
    invokeClick(harness.buttons[0]);
    expect(onSelectAll).toHaveBeenCalledWith(false);
  });

  it("renders the unstage and destructive delete variants while collapsed", () => {
    const { markup } = renderHeader(
      {
        primaryAction: { icon: "unstage", label: "Unstage all", onClick: vi.fn() },
        onDiscard: vi.fn(),
        discardVariant: "delete-untracked",
        disabled: true,
      },
      { collapsed: true },
    );
    expect(markup).toContain("-rotate-90");
    expect(markup).toContain('aria-expanded="false"');
    expect(harness.buttons[0]).toMatchObject({ "aria-label": "Unstage all", disabled: true });
    expect(harness.buttons[1]).toMatchObject({
      "aria-label": "Delete all untracked",
      title: "Delete all untracked",
      disabled: true,
    });
    expect(String(harness.buttons[1]?.className)).toContain("text-destructive");
  });
});
