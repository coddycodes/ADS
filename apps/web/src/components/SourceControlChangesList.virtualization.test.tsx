// @vitest-environment happy-dom

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { SourceControlChangesList } from "./SourceControlChangesList";
import { buildSourceControlRows } from "./SourceControlChangesList.logic";
import { SourceControlMergeChanges } from "./SourceControlMergeChanges";
import { isFileStaged, type WorkingTreeFile } from "./SourceControlPanel.logic";

// happy-dom has no layout, so every element reports a 300x600 box and the list
// sizes its window from that viewport.
const VIEWPORT = { width: 300, height: 600 };
// A 600 px viewport holds under 25 rows plus a small overscan. Anything near the
// file count means rows mount without a viewport bound.
const MOUNTED_ROW_BOUND = 120;

function changedFiles(count: number): WorkingTreeFile[] {
  return Array.from({ length: count }, (_, index) => ({
    path: `packages/pkg-${index % 37}/src/file-${index}.ts`,
    insertions: index % 90,
    deletions: 1,
    status: "modified" as const,
    area: "unstaged" as const,
  }));
}

let host: HTMLDivElement;
let root: Root;

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue(
    DOMRect.fromRect({ x: 0, y: 0, ...VIEWPORT }),
  );
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.restoreAllMocks();
});

async function settle(): Promise<void> {
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 20));
  });
}

function mountedFileRows(): number {
  return host.querySelectorAll('[aria-label^="Stage packages/"]').length;
}

describe("Source Control changes list virtualization (#119)", () => {
  it("mounts a viewport-bounded number of rows for thousands of changes", async () => {
    const files = changedFiles(4_000);
    await act(async () => {
      root.render(
        <div style={{ height: VIEWPORT.height }}>
          <SourceControlChangesList
            sections={[
              {
                title: "Changes",
                files,
                checked: isFileStaged,
                onToggle: () => undefined,
                onOpenFile: () => undefined,
              },
            ]}
          />
        </div>,
      );
    });
    await settle();

    const mounted = mountedFileRows();
    expect(mounted).toBeGreaterThan(0);
    expect(mounted).toBeLessThan(MOUNTED_ROW_BOUND);
  });

  it("keeps mounted rows in screen order after scrolling, so Tab follows the screen", async () => {
    const files = changedFiles(4_000);
    const section = {
      title: "Changes",
      files,
      checked: isFileStaged,
      onToggle: () => undefined,
      onOpenFile: () => undefined,
    };
    await act(async () => {
      root.render(<SourceControlChangesList sections={[section]} />);
    });
    const scroller = host.querySelector<HTMLElement>(".overflow-y-auto");
    if (!scroller) throw new Error("Missing scroller");
    await act(async () => {
      scroller.scrollTop = 50_000;
      scroller.dispatchEvent(new Event("scroll"));
    });

    const expected = buildSourceControlRows([section], {
      groupByFolder: true,
      collapsedSections: new Set(),
      collapsedFolders: new Set(),
    }).flatMap((row) => (row.kind === "file" ? [`Stage ${row.file.path}`] : []));
    const mounted = [...host.querySelectorAll('[aria-label^="Stage packages/"]')].map((element) =>
      element.getAttribute("aria-label"),
    );
    const first = expected.indexOf(mounted[0] ?? "");
    expect(first).toBeGreaterThan(0);
    expect(mounted).toEqual(expected.slice(first, first + mounted.length));
  });

  it("keeps the focused row mounted while it scrolls out of the window", async () => {
    const section = {
      title: "Changes",
      files: changedFiles(4_000),
      checked: isFileStaged,
      onToggle: () => undefined,
      onOpenFile: () => undefined,
    };
    await act(async () => {
      root.render(<SourceControlChangesList sections={[section]} />);
    });
    const open = host.querySelector<HTMLButtonElement>('button[title^="packages/"]');
    if (!open) throw new Error("Missing row");
    await act(async () => open.focus());
    const scroller = host.querySelector<HTMLElement>(".overflow-y-auto");
    if (!scroller) throw new Error("Missing scroller");
    await act(async () => {
      scroller.scrollTop = 50_000;
      scroller.dispatchEvent(new Event("scroll"));
    });

    expect(open.isConnected).toBe(true);
    expect(document.activeElement).toBe(open);
    expect(mountedFileRows()).toBeLessThan(MOUNTED_ROW_BOUND);
  });

  it("mounts a viewport-bounded number of conflict rows during a large merge", async () => {
    const files = changedFiles(4_000).map((file) => ({ ...file, status: "conflicted" as const }));
    await act(async () => {
      root.render(
        <SourceControlMergeChanges
          disabled={false}
          files={files}
          onMarkResolved={() => undefined}
          onOpenInEditor={() => undefined}
          onResolve={() => undefined}
        />,
      );
    });
    await settle();

    const mounted = host.querySelectorAll('[aria-label^="Keep ours for packages/"]').length;
    expect(mounted).toBeGreaterThan(0);
    expect(mounted).toBeLessThan(MOUNTED_ROW_BOUND);
  });
});
