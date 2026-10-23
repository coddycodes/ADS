// @vitest-environment happy-dom

import { EnvironmentId } from "@bibcode/contracts";
import { AsyncResult } from "effect/reactivity";
import { act, type ReactElement } from "react";
import { createRoot, type Root } from "react-dom/client";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

interface BrowseResultFixture {
  readonly parentPath: string;
  readonly directoryPath: string;
  readonly ancestorPath: string;
  readonly breadcrumbs: ReadonlyArray<{ readonly name: string; readonly path: string }>;
  readonly entries: ReadonlyArray<{ readonly name: string; readonly path: string }>;
}

const h = vi.hoisted(() => ({
  browseState: { result: null as BrowseResultFixture | null },
  refresh: vi.fn(),
  createEntry: vi.fn(),
  reset() {
    this.browseState.result = null;
    this.refresh.mockReset();
    this.createEntry
      .mockReset()
      .mockResolvedValue(AsyncResult.success({ relativePath: "new-folder" }));
  },
}));

vi.mock("~/state/filesystem", () => ({
  filesystemEnvironment: {
    browse: (target: { environmentId: string; input: { partialPath: string; mode: string } }) =>
      target,
  },
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (target: unknown) => {
    if (target === null)
      return {
        data: null,
        error: null,
        isPending: false,
        refresh: h.refresh,
        revalidate: h.refresh,
        requiresRetry: false,
      };
    const result = h.browseState.result;
    return {
      data:
        result === null
          ? null
          : {
              directoryPath: result.directoryPath,
              ancestorPath: result.ancestorPath,
              breadcrumbs: result.breadcrumbs.map((breadcrumb) => ({
                name: breadcrumb.name,
                fullPath: breadcrumb.path,
              })),
              entries: result.entries.map((entry) => ({
                name: entry.name,
                fullPath: entry.path,
              })),
            },
      error: null,
      isPending: false,
      refresh: h.refresh,
      revalidate: h.refresh,
      requiresRetry: false,
    };
  },
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: { createEntry: "project-create-entry" },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: () => h.createEntry,
}));

const { RemoteDirectoryBrowser } = await import("./RemoteDirectoryBrowser");

const ENV = EnvironmentId.make("environment-one");

interface Mounted {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

async function mount(element: ReactElement): Promise<Mounted> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  await act(async () => root.render(element));
  return { container, root };
}

function requiredButton(container: ParentNode, label: string): HTMLButtonElement {
  const found = Array.from(container.querySelectorAll("button")).find(
    (candidate) => candidate.textContent?.trim() === label,
  );
  if (!found) throw new Error(`Missing button: ${label}`);
  return found;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  h.reset();
});

describe("RemoteDirectoryBrowser", () => {
  it("selects the canonical directory and exposes the secondary action", async () => {
    h.browseState.result = {
      parentPath: "/srv",
      directoryPath: "/srv/code",
      ancestorPath: "/srv",
      breadcrumbs: [
        { name: "srv", path: "/srv" },
        { name: "code", path: "/srv/code" },
      ],
      entries: [{ name: "app", path: "/srv/code/app" }],
    };
    const onSelect = vi.fn();
    const onCancel = vi.fn();
    const secondary = vi.fn();
    const { container, root } = await mount(
      <RemoteDirectoryBrowser
        environmentId={ENV}
        initialPath="/srv/code"
        resetKey={true}
        onSelect={onSelect}
        onCancel={onCancel}
        secondaryAction={{ label: "Type a path instead", onClick: secondary }}
      />,
    );

    await act(async () => requiredButton(container, "Type a path instead").click());
    expect(secondary).toHaveBeenCalledOnce();

    await act(async () => requiredButton(container, "Select folder").click());
    expect(onSelect).toHaveBeenCalledWith("/srv/code");

    await act(async () => requiredButton(container, "Cancel").click());
    expect(onCancel).toHaveBeenCalledOnce();

    await act(async () => root.unmount());
    container.remove();
  });
});
