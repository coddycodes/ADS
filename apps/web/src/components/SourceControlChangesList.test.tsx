import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vite-plus/test";

import {
  SourceControlChangesList,
  type SourceControlListSection,
} from "./SourceControlChangesList";
import type { WorkingTreeFile } from "./SourceControlPanel.logic";

function renderList(
  section: Partial<SourceControlListSection> & { files: readonly WorkingTreeFile[] },
  groupByFolder?: boolean,
): string {
  return renderToStaticMarkup(
    <SourceControlChangesList
      {...(groupByFolder === undefined ? {} : { groupByFolder })}
      sections={[{ title: "Changes", onToggle: () => {}, onOpenFile: () => {}, ...section }]}
    />,
  );
}

const FILES: WorkingTreeFile[] = [
  { path: "docs/prps/PFS-1848/master-plan.md", insertions: 218, deletions: 0 },
  { path: "tasks.md", insertions: 139, deletions: 4 },
];

const STAGED_FILE: WorkingTreeFile = {
  path: "src/app.ts",
  insertions: 3,
  deletions: 1,
  status: "modified",
  area: "staged",
};
const UNSTAGED_FILE: WorkingTreeFile = {
  path: "src/util.ts",
  insertions: 2,
  deletions: 0,
  status: "modified",
};
const DELETED_FILE: WorkingTreeFile = {
  path: "src/old.ts",
  insertions: 0,
  deletions: 9,
  status: "deleted",
};
const UNTRACKED_FILE: WorkingTreeFile = {
  path: "src/new.ts",
  insertions: 5,
  deletions: 0,
  status: "untracked",
  area: "untracked",
};

function checkboxCount(markup: string): number {
  return markup.match(/data-slot="checkbox"/g)?.length ?? 0;
}

// FILES spans two folders, so the grouped view renders one header checkbox per
// folder on top of the per-row ones.
const GROUPED_CHECKBOX_COUNT = FILES.length * 2;

describe("SourceControlChangesList", () => {
  it("renders each file's name under its folder header with +/- counts", () => {
    const markup = renderList({ files: FILES });
    expect(markup).toContain("master-plan.md");
    expect(markup).toContain("docs/prps/PFS-1848");
    expect(markup).toContain("+218");
    expect(markup).toContain("-4");
  });

  it("groups files under expandable folder headers, root first", () => {
    const markup = renderList({ files: FILES });
    expect(markup).toContain('title="Repository root"');
    expect(markup).toContain('title="docs/prps/PFS-1848"');
    // The section header and both folder headers are expanded.
    expect(markup.match(/aria-expanded="true"/g)).toHaveLength(3);
    expect(markup.indexOf("Repository root")).toBeLessThan(markup.indexOf("docs/prps/PFS-1848"));
    expect(markup).toContain('aria-label="docs/prps/PFS-1848, 1 file"');
    // The path is isolated with <bdi>, so no bidi control characters end up in
    // the text the user can select and copy.
    expect(markup).toContain('<bdi dir="ltr">docs/prps/PFS-1848</bdi>');
    expect(markup).not.toContain("\u200e");
  });

  it("renders one flat row per file with its own directory when grouping is off", () => {
    const markup = renderList({ files: FILES }, false);
    // Only the section header expands; no folder headers render.
    expect(markup.match(/aria-expanded/g)).toHaveLength(1);
    expect(markup).not.toContain("Repository root");
    expect(markup).toContain("docs/prps/PFS-1848");
    expect(markup).toContain("master-plan.md");
    // The directory keeps a readable floor and yields the rest to the name,
    // which is never capped.
    expect(markup).toContain("min-w-[4.5rem]");
    expect(markup).not.toContain("max-w-[");
  });

  it("renders a folder header checkbox that stages the whole folder", () => {
    const markup = renderList({ files: FILES, checked: () => false });
    expect(markup).toContain('aria-label="Stage all files in docs/prps/PFS-1848"');
    expect(markup).toContain('aria-label="Stage all files in the repository root"');
  });

  it("renders each file's status letter", () => {
    const markup = renderList({ files: [UNTRACKED_FILE, STAGED_FILE] });
    expect(markup).toContain('title="Untracked"');
    expect(markup).toContain('title="Modified"');
  });

  it("renders an empty state when no section has files", () => {
    const markup = renderList({ files: [] });
    expect(markup).toContain("No changes");
  });

  it("renders no checkbox when `checked` is omitted (legacy server)", () => {
    const markup = renderList({ files: FILES });
    expect(markup).not.toContain('data-slot="checkbox"');
  });

  it('renders a checked box labeled "Unstage <path>" for a staged file', () => {
    const markup = renderList({ files: FILES, checked: () => true });
    expect(markup).toContain('data-slot="checkbox"');
    expect(checkboxCount(markup)).toBe(GROUPED_CHECKBOX_COUNT);
    expect(markup).toContain(`aria-label="Unstage ${FILES[0]!.path}"`);
    expect(markup).toContain('data-checked=""');
  });

  it('renders an unchecked box labeled "Stage <path>" for an unstaged file', () => {
    const markup = renderList({ files: FILES, checked: () => false });
    expect(markup).toContain('data-slot="checkbox"');
    expect(checkboxCount(markup)).toBe(GROUPED_CHECKBOX_COUNT);
    expect(markup).toContain(`aria-label="Stage ${FILES[0]!.path}"`);
    expect(markup).toContain('data-unchecked=""');
  });

  it("renders selection instead of staging when selection mode is active", () => {
    const markup = renderList({
      files: FILES,
      selectionMode: true,
      checked: () => false,
      selected: (file) => file.path === FILES[0]?.path,
      onSelect: () => {},
    });

    expect(checkboxCount(markup)).toBe(GROUPED_CHECKBOX_COUNT);
    expect(markup).toContain(`aria-label="Deselect ${FILES[0]!.path}"`);
    expect(markup).not.toContain(`aria-label="Stage ${FILES[0]!.path}"`);
  });

  it("renders no inline action buttons when no row-action handlers are wired", () => {
    const markup = renderList({ files: [STAGED_FILE] });
    expect(markup).not.toContain(`aria-label="Unstage ${STAGED_FILE.path}"`);
  });

  it("renders a single Unstage inline action for a staged row", () => {
    const markup = renderList({
      files: [STAGED_FILE],
      onUnstageFile: () => {},
      onStageFile: () => {},
    });
    expect(markup).toContain(`aria-label="Unstage ${STAGED_FILE.path}"`);
    expect(markup).not.toContain(`aria-label="Stage ${STAGED_FILE.path}"`);
  });

  it("renders Discard then Stage inline actions for an unstaged row", () => {
    const markup = renderList({
      files: [UNSTAGED_FILE],
      onRequestDiscardFile: () => {},
      onStageFile: () => {},
    });
    expect(markup).toContain(`aria-label="Discard changes ${UNSTAGED_FILE.path}"`);
    expect(markup).toContain(`aria-label="Stage ${UNSTAGED_FILE.path}"`);
  });

  it('labels the discard action "Restore file" for a deleted unstaged row', () => {
    const markup = renderList({ files: [DELETED_FILE], onRequestDiscardFile: () => {} });
    expect(markup).toContain(`aria-label="Restore file ${DELETED_FILE.path}"`);
  });

  it("renders a destructive Delete inline action for an untracked row", () => {
    const markup = renderList({
      files: [UNTRACKED_FILE],
      onRequestDiscardFile: () => {},
      onStageFile: () => {},
    });
    expect(markup).toContain(`aria-label="Delete untracked file ${UNTRACKED_FILE.path}"`);
    expect(markup).toContain(`aria-label="Stage ${UNTRACKED_FILE.path}"`);
    expect(markup).toContain("text-destructive");
  });
});
