// @vitest-environment happy-dom

import type { GitManagerRefEntry, GitManagerRemoteTags } from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const h = vi.hoisted(() => ({
  remoteTags: null as GitManagerRemoteTags | null,
  pending: false,
  error: null as string | null,
  refresh: vi.fn(),
  getRemoteTags: vi.fn((input: unknown) => ({ kind: "remote-tags", input })),
}));

vi.mock("../../../state/gitManager", () => ({
  gitManagerEnvironment: { getRemoteTags: h.getRemoteTags },
}));

vi.mock("../../../state/query", () => ({
  useEnvironmentQuery: () => ({
    data: h.remoteTags,
    emission: { _tag: h.remoteTags === null ? "Initial" : "Success" },
    error: h.error,
    isPending: h.pending,
    refresh: h.refresh,
  }),
}));

vi.mock("~/components/ui/collapsible", () => ({
  Collapsible: ({
    children,
    open,
    onOpenChange,
  }: {
    children: React.ReactNode;
    open: boolean;
    onOpenChange: (open: boolean) => void;
  }) => (
    <div data-open={open} data-testid="collapsible">
      <button type="button" onClick={() => onOpenChange(!open)}>
        toggle
      </button>
      {children}
    </div>
  ),
  CollapsibleTrigger: ({ children, ...props }: Record<string, unknown>) => (
    <span aria-label={props["aria-label"] as string | undefined}>
      {children as React.ReactNode}
    </span>
  ),
  CollapsiblePanel: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}));

import { GitManagerTagsView } from "./GitManagerTagsView";

const SHA_A = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA_B = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

function tag(name: string, tipSha: string): GitManagerRefEntry {
  return {
    name,
    tipSha,
    upstream: null,
    ahead: 0,
    behind: 0,
    current: false,
    isDefault: false,
    worktreePath: null,
    blocked: [],
  };
}

let container: HTMLDivElement;
let root: Root | null;
const onSectionCollapsedChange = vi.fn();
const onTagAction = vi.fn();

async function renderView(
  overrides: Partial<React.ComponentProps<typeof GitManagerTagsView>> = {},
) {
  await act(async () =>
    root?.render(
      <GitManagerTagsView
        collapsedSections={[]}
        remotes={["origin"]}
        scope={{ environmentId: "env-a" as never, cwd: "/repo" }}
        tagDisabledReason={null}
        tags={[tag("v1", SHA_A), tag("v2", SHA_B)]}
        onSectionCollapsedChange={onSectionCollapsedChange}
        onTagAction={onTagAction}
        {...overrides}
      />,
    ),
  );
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  h.remoteTags = null;
  h.pending = false;
  h.error = null;
  h.refresh.mockClear();
  h.getRemoteTags.mockClear();
  onSectionCollapsedChange.mockClear();
  onTagAction.mockClear();
});

afterEach(async () => {
  await act(async () => root?.unmount());
  root = null;
  container.remove();
});

describe("GitManagerTagsView", () => {
  it("lists local tags newest first with push and delete actions", async () => {
    await renderView();
    const names = [...container.querySelectorAll('[data-testid^="git-manager-local-tag-"]')].map(
      (row) => row.getAttribute("data-testid"),
    );
    expect(names).toEqual(["git-manager-local-tag-v2", "git-manager-local-tag-v1"]);
    container.querySelector<HTMLButtonElement>('[aria-label="Push tag v2"]')!.click();
    container.querySelector<HTMLButtonElement>('[aria-label="Delete tag v1"]')!.click();
    expect(onTagAction.mock.calls).toEqual([
      ["push", "v2"],
      ["delete", "v1"],
    ]);
  });

  it("queries each remote for its tags and marks presence against local tags", async () => {
    h.remoteTags = {
      remote: "origin",
      status: "available",
      reason: null,
      tags: [
        { name: "v1", targetSha: SHA_A },
        { name: "v3", targetSha: SHA_B },
      ],
    };
    await renderView();
    expect(h.getRemoteTags).toHaveBeenCalledWith({
      environmentId: "env-a",
      input: { cwd: "/repo", remote: "origin" },
    });
    expect(
      container
        .querySelector('[data-testid="git-manager-remote-tag-origin-v3"]')
        ?.getAttribute("data-presence"),
    ).toBe("not-fetched");
    expect(
      container
        .querySelector('[data-testid="git-manager-remote-tag-origin-v1"]')
        ?.getAttribute("data-presence"),
    ).toBe("same");
    expect(container.textContent).toContain("not fetched");
    container.querySelector<HTMLButtonElement>('[aria-label="Refresh tags from origin"]')!.click();
    expect(h.refresh).toHaveBeenCalledOnce();
  });

  it("reports an unavailable remote with its reason and a retry", async () => {
    h.remoteTags = { remote: "origin", status: "unavailable", reason: "no route", tags: [] };
    await renderView();
    expect(container.textContent).toContain("no route");
    const retry = [...container.querySelectorAll("button")].find(
      (button) => button.textContent === "Retry",
    );
    retry!.click();
    expect(h.refresh).toHaveBeenCalledOnce();
  });

  it("reflects collapsed sections and reports toggles by section key", async () => {
    await renderView({ collapsedSections: ["remote:origin"] });
    const sections = [...container.querySelectorAll('[data-testid="collapsible"]')];
    expect(sections.map((section) => section.getAttribute("data-open"))).toEqual(["true", "false"]);
    sections[0]!.querySelector("button")!.click();
    expect(onSectionCollapsedChange).toHaveBeenCalledWith("local", true);
  });

  it("explains the absence of remotes and hides push actions", async () => {
    await renderView({ remotes: [] });
    expect(container.textContent).toContain("No remote configured");
    expect(container.querySelector('[aria-label="Push tag v2"]')).toBeNull();
  });
});
