// @vitest-environment happy-dom

import type {
  GitManagerRefEntry,
  GitManagerOperationEvent,
  GitManagerRefsSnapshot,
  VcsWorktreeDescriptor,
} from "@bibcode/contracts";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { beforeEach, describe, expect, it, vi } from "vite-plus/test";

import { useGitManagerStore } from "../../gitManagerStore";

const h = vi.hoisted(() => ({
  selectProps: null as Record<string, unknown> | null,
  itemProps: [] as Array<Record<string, unknown>>,
  snapshot: null as GitManagerRefsSnapshot | null,
  refreshRefs: vi.fn(),
  tagDialogProps: [] as Array<Record<string, unknown>>,
  branchDialogProps: [] as Array<Record<string, unknown>>,
  menuItemProps: [] as Array<Record<string, unknown>>,
  refsAtom: vi.fn(() => ({ kind: "refs" })),
  signalAtom: vi.fn(() => ({ kind: "signal" })),
  runOperation: vi.fn(),
  failureMessage: null as string | null,
  /** Failures for one operation tag only; `failureMessage` fails every operation. */
  failures: {} as Record<string, { readonly code: string; readonly message: string }>,
}));

vi.mock("../../state/gitManager", () => ({
  runGitManagerOperation: h.runOperation,
  gitManagerEnvironment: {
    getRefs: h.refsAtom,
    signalWithDegradedFocusRefresh: h.signalAtom,
  },
}));

vi.mock("@legendapp/list/react", () => ({
  LegendList: ({
    data,
    renderItem,
    keyExtractor,
  }: {
    data: ReadonlyArray<unknown>;
    renderItem: (input: { item: unknown; index: number }) => React.ReactNode;
    keyExtractor: (item: unknown) => string;
  }) => (
    <>
      {data.map((item, index) => (
        <div key={keyExtractor(item)}>{renderItem({ item, index })}</div>
      ))}
    </>
  ),
}));

vi.mock("~/components/ui/popover", () => ({
  Popover: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  PopoverTrigger: ({ children, ...props }: React.ComponentProps<"button">) => (
    <button {...props}>{children}</button>
  ),
  PopoverPopup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}));

vi.mock("../../state/entities", () => ({
  useProject: () => ({ id: "project-current", title: "pathfinder-docker" }),
}));

vi.mock("../../state/environments", () => ({
  useEnvironment: () => ({
    environmentId: "env-a",
    label: "ai-server",
    displayUrl: "http://ai-server:3773",
  }),
  usePrimaryEnvironmentId: () => "env-primary",
}));

vi.mock("../../state/query", () => ({
  useEnvironmentQuery: (atom: { kind: string } | null) => ({
    data: atom?.kind === "refs" ? h.snapshot : null,
    error: null,
    isPending: false,
    refresh: h.refreshRefs,
    revalidate: h.refreshRefs,
    requiresRetry: false,
  }),
}));

vi.mock("../ui/select", () => ({
  Select: (props: Record<string, unknown>) => {
    h.selectProps = props;
    return <div>{props.children as React.ReactNode}</div>;
  },
  SelectTrigger: (props: Record<string, unknown>) => (
    <button aria-label={props["aria-label"] as string} disabled={props.disabled as boolean}>
      {props.children as React.ReactNode}
    </button>
  ),
  SelectValue: () => <span>Selected worktree</span>,
  SelectPopup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  SelectGroup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  SelectGroupLabel: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
  SelectItem: (props: Record<string, unknown>) => {
    h.itemProps.push(props);
    return <button>{props.children as React.ReactNode}</button>;
  },
}));

vi.mock("../ui/menu", () => ({
  Menu: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  MenuTrigger: ({ children, ...props }: Record<string, unknown>) => (
    <button {...props}>{children as React.ReactNode}</button>
  ),
  MenuPopup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  MenuItem: (props: Record<string, unknown>) => {
    h.menuItemProps.push(props);
    return (
      <button
        aria-label={props["aria-label"] as string | undefined}
        disabled={props.disabled as boolean}
        onClick={props.onClick as React.MouseEventHandler<HTMLButtonElement>}
      >
        {props.children as React.ReactNode}
      </button>
    );
  },
  MenuSeparator: () => <hr />,
  MenuSub: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
  MenuSubTrigger: ({ children, ...props }: Record<string, unknown>) => (
    <button disabled={props.disabled as boolean}>{children as React.ReactNode}</button>
  ),
  MenuSubPopup: ({ children }: { children: React.ReactNode }) => <div>{children}</div>,
}));

vi.mock("./tags/GitManagerTagDialog", () => ({
  GitManagerTagDialog: (props: Record<string, unknown>) => {
    h.tagDialogProps.push(props);
    return props.open === true ? <div role="dialog">{String(props.action)} tag dialog</div> : null;
  },
}));

vi.mock("./dialogs/GitManagerBranchDialogs", () => ({
  GitManagerBranchDialogs: (props: Record<string, unknown>) => {
    h.branchDialogProps.push(props);
    return null;
  },
}));

import { GitManagerToolbar } from "./GitManagerToolbar";

const currentProject = { environmentId: "env-a", projectId: "project-current" } as never;
const otherProject = { environmentId: "env-a", projectId: "project-other" } as never;
const worktrees = [
  { path: "/opaque/main", branch: "main", isPrimary: true },
  { path: "/opaque/feature", branch: "feature", isPrimary: false },
] as unknown as ReadonlyArray<VcsWorktreeDescriptor>;

function ref(name: string, overrides: Partial<GitManagerRefEntry> = {}): GitManagerRefEntry {
  return {
    name,
    tipSha: "a".repeat(40),
    upstream: null,
    ahead: 0,
    behind: 0,
    current: false,
    isDefault: false,
    worktreePath: null,
    blocked: [],
    ...overrides,
  };
}

function refsSnapshot(tags: ReadonlyArray<GitManagerRefEntry> = []): GitManagerRefsSnapshot {
  return {
    generation: 1,
    headRef: "main",
    detachedSha: null,
    isDirty: false,
    defaultBranch: "main",
    remotes: ["origin"],
    localBranches: [ref("main", { current: true, isDefault: true, upstream: "origin/main" })],
    remoteBranches: [ref("origin/main")],
    tags,
    worktrees: [],
    inProgressOperation: null,
    conflictedPaths: [],
  };
}

function renderToolbar(
  overrides: Partial<React.ComponentProps<typeof GitManagerToolbar>> = {},
): string {
  return renderToStaticMarkup(
    <GitManagerToolbar
      projectRef={currentProject}
      mainCheckoutCwd="/opaque/main"
      selectedWorktreeCwd="/opaque/main"
      worktrees={worktrees}
      catalogPending={false}
      catalogError={null}
      repositoryUnavailable={null}
      branchSyncDisabledReason={null}
      stashMergeDisabledReason={null}
      tagDisabledReason={null}
      onSelectedWorktreeChange={() => undefined}
      {...overrides}
    />,
  );
}

beforeEach(() => {
  h.selectProps = null;
  h.itemProps.length = 0;
  h.snapshot = null;
  h.refreshRefs.mockClear();
  h.tagDialogProps.length = 0;
  h.branchDialogProps.length = 0;
  h.menuItemProps.length = 0;
  h.refsAtom.mockClear();
  h.signalAtom.mockClear();
  h.runOperation.mockReset();
  h.failureMessage = null;
  h.failures = {};
  h.runOperation.mockImplementation((_registry, { input }, onEvent) => {
    const failure =
      h.failures[input._tag] ??
      (h.failureMessage === null ? null : { code: "branch-exists", message: h.failureMessage });
    const event: GitManagerOperationEvent =
      failure === null
        ? {
            _tag: "finished",
            operation: input._tag,
            message: "Finished.",
          }
        : {
            _tag: "failed",
            operation: input._tag,
            code: failure.code,
            message: failure.message,
            blocked: null,
          };
    onEvent(event);
    return { cancel: vi.fn(), result: Promise.resolve({ _tag: "Success", value: event }) };
  });
  useGitManagerStore.setState({ byProjectKey: {} });
});

describe("GitManagerToolbar", () => {
  it.each([
    ["origin/develop", true, null, false],
    ["origin/develop", true, "bring", false],
    ["origin/develop", true, "stash", false],
    ["refs/remotes/origin/develop", false, null, false],
    ["origin/develop", true, "bring", true],
  ] as const)(
    "checks out %s (remote: %s, strategy: %s, failure: %s) on its environment",
    async (branchName, isRemote, strategy, fails) => {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
      h.snapshot = {
        ...refsSnapshot(),
        isDirty: strategy !== null,
        localBranches: [...refsSnapshot().localBranches, ...(isRemote ? [] : [ref(branchName)])],
        remoteBranches: [ref("origin/develop")],
      };
      if (fails)
        h.failureMessage =
          "A local branch named 'develop' already exists. Select it in the local branches list.";
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      try {
        await act(async () =>
          root.render(
            <GitManagerToolbar
              projectRef={currentProject}
              mainCheckoutCwd="/opaque/main"
              selectedWorktreeCwd="/opaque/main"
              worktrees={worktrees}
              catalogPending={false}
              catalogError={null}
              repositoryUnavailable={null}
              branchSyncDisabledReason={null}
              stashMergeDisabledReason={null}
              tagDisabledReason={null}
              onSelectedWorktreeChange={() => undefined}
            />,
          ),
        );
        const click = async (text: string) => {
          const button = [...document.querySelectorAll("button")].find(
            (entry) => entry.textContent === text,
          );
          expect(button).toBeDefined();
          await act(async () => button!.click());
        };
        await click(branchName);
        if (strategy !== null) {
          expect(h.runOperation).not.toHaveBeenCalled();
          expect(document.body.textContent).toContain("Switch to origin/develop?");
          await click(strategy === "bring" ? "Bring my changes" : "Leave my changes");
        }
        const targets = h.runOperation.mock.calls.map((call) => call[1]);
        expect(targets.at(-1)).toEqual({
          environmentId: "env-a",
          input: {
            _tag: "branch-checkout",
            cwd: "/opaque/main",
            projectId: "project-current",
            name: `refs/${isRemote ? "remotes" : "heads"}/${branchName}`,
            strategy: strategy === "bring" ? "bring" : null,
          },
        });
        expect(targets.map((target) => target.input._tag)).toEqual(
          strategy === "stash" ? ["stash-push", "branch-checkout"] : ["branch-checkout"],
        );
        if (strategy === "stash") {
          // Leave my changes must save untracked files too, or the switch stays blocked.
          expect(targets[0]?.input).toEqual({
            _tag: "stash-push",
            cwd: "/opaque/main",
            projectId: "project-current",
            message: "WIP before switching to origin/develop",
            paths: [],
            includeUntracked: true,
          });
        }
        expect(h.refreshRefs).toHaveBeenCalled();
        if (fails) {
          expect(document.querySelector('[role="dialog"] [role="alert"]')?.textContent).toBe(
            h.failureMessage,
          );
          expect(
            useGitManagerStore.getState().selectViewState(currentProject).selectedRef,
          ).toBeNull();
          h.failureMessage = null;
          await click("Bring my changes");
          expect(document.querySelector('[role="dialog"]')).toBeNull();
        }
        expect(useGitManagerStore.getState().selectViewState(currentProject).selectedRef).toBe(
          isRemote ? "develop" : branchName,
        );
      } finally {
        await act(async () => root.unmount());
        container.remove();
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
      }
    },
  );

  it.each([
    [
      "the stash saves nothing",
      "stash-push",
      "nothing-to-stash",
      "Git found no changes it can stash, so nothing was saved.",
      ["stash-push"],
      "Git found no changes it can stash, so nothing was saved.",
    ],
    [
      "changes Git can't stash remain",
      "branch-checkout",
      "dirty-working-tree",
      "Checkout is blocked: the working tree has uncommitted changes.",
      ["stash-push", "branch-checkout"],
      "Your changes are saved in the stash “WIP before switching to origin/develop”, but some changes are still here (Git can't stash changes inside a submodule), so the branch was not switched.",
    ],
    [
      "the switch fails for another reason",
      "branch-checkout",
      "branch-exists",
      "A local branch named 'develop' already exists.",
      ["stash-push", "branch-checkout"],
      "Your changes are saved in the stash “WIP before switching to origin/develop”. A local branch named 'develop' already exists.",
    ],
  ] as const)(
    "keeps Leave my changes open and explains when %s",
    async (_case, failingTag, code, message, expectedTags, expectedAlert) => {
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
      h.snapshot = { ...refsSnapshot(), isDirty: true, remoteBranches: [ref("origin/develop")] };
      h.failures = { [failingTag]: { code, message } };
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      try {
        await act(async () =>
          root.render(
            <GitManagerToolbar
              projectRef={currentProject}
              mainCheckoutCwd="/opaque/main"
              selectedWorktreeCwd="/opaque/main"
              worktrees={worktrees}
              catalogPending={false}
              catalogError={null}
              repositoryUnavailable={null}
              branchSyncDisabledReason={null}
              stashMergeDisabledReason={null}
              tagDisabledReason={null}
              onSelectedWorktreeChange={() => undefined}
            />,
          ),
        );
        const click = async (text: string) => {
          const button = [...document.querySelectorAll("button")].find(
            (entry) => entry.textContent === text,
          );
          expect(button).toBeDefined();
          await act(async () => button!.click());
        };
        await click("origin/develop");
        await click("Leave my changes");

        expect(h.runOperation.mock.calls.map((call) => call[1].input._tag)).toEqual(expectedTags);
        expect(document.querySelector('[role="dialog"] [role="alert"]')?.textContent).toBe(
          expectedAlert,
        );
        expect(
          useGitManagerStore.getState().selectViewState(currentProject).selectedRef,
        ).toBeNull();
      } finally {
        await act(async () => root.unmount());
        container.remove();
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
      }
    },
  );

  it("opens on the current project's main checkout, not another project's cached worktree", () => {
    useGitManagerStore.getState().setSelectedWorktree(otherProject, "/opaque/other-worktree");

    renderToolbar();

    expect(h.selectProps?.value).toBe("/opaque/main");
    expect(h.itemProps.map((props) => props.value)).toEqual(["/opaque/main", "/opaque/feature"]);
  });

  it("names the project it manages ahead of the worktree selector", () => {
    const markup = renderToolbar();

    expect(markup).toContain('data-testid="git-manager-project"');
    expect(markup).toContain("pathfinder-docker");
    expect(markup).toContain('title="/opaque/main"');
    expect(markup).toContain('data-testid="git-manager-environment"');
    expect(markup).toContain("ai-server");
    expect(markup).toContain('title="http://ai-server:3773"');
  });

  it("persists an explicit worktree selection through its callback", () => {
    const onSelectedWorktreeChange = vi.fn();
    renderToolbar({ onSelectedWorktreeChange });

    (h.selectProps?.onValueChange as ((value: string | null) => void) | undefined)?.(
      "/opaque/feature",
    );
    expect(onSelectedWorktreeChange).toHaveBeenCalledWith("/opaque/feature");
  });

  it("renders the branch selector and an accessible sync loading state", () => {
    const markup = renderToolbar();

    expect(markup).toContain('aria-label="Choose branch"');
    expect(markup).toContain("Loading repository state…");
    expect(markup).toContain('title="Loading repository state."');
    expect(markup).toContain('title="Loading tags."');
    expect(markup).toContain('aria-describedby="git-manager-tag-trigger-reason"');
  });

  it("opens create, delete, and push tag dialogs from the toolbar", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.snapshot = refsSnapshot([ref("release/v1")]);
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () =>
        root.render(
          <GitManagerToolbar
            projectRef={currentProject}
            mainCheckoutCwd="/opaque/main"
            selectedWorktreeCwd="/opaque/main"
            worktrees={worktrees}
            catalogPending={false}
            catalogError={null}
            repositoryUnavailable={null}
            branchSyncDisabledReason={null}
            stashMergeDisabledReason={null}
            tagDisabledReason={null}
            onSelectedWorktreeChange={() => undefined}
          />,
        ),
      );
      const clickAction = async (label: string) => {
        const action = container.querySelector<HTMLButtonElement>(`button[aria-label="${label}"]`);
        expect(action).not.toBeNull();
        await act(async () => action?.click());
      };

      await clickAction("Create tag");
      expect(container.querySelector('[role="dialog"]')?.textContent).toBe("create tag dialog");
      expect(h.tagDialogProps.at(-1)).toMatchObject({
        action: "create",
        existingTags: ["release/v1"],
        targetSha: "a".repeat(40),
      });

      const closeCreate = h.tagDialogProps.at(-1)?.onOpenChange;
      expect(closeCreate).toBeTypeOf("function");
      await act(async () => (closeCreate as (open: boolean) => void)(false));
      await clickAction("Delete tag release/v1");
      expect(container.querySelector('[role="dialog"]')?.textContent).toBe("delete tag dialog");
      expect(h.tagDialogProps.at(-1)).toMatchObject({ action: "delete", tag: "release/v1" });

      const closeDelete = h.tagDialogProps.at(-1)?.onOpenChange;
      expect(closeDelete).toBeTypeOf("function");
      await act(async () => (closeDelete as (open: boolean) => void)(false));
      await clickAction("Push tag release/v1");
      expect(container.querySelector('[role="dialog"]')?.textContent).toBe("push tag dialog");
      expect(h.tagDialogProps.at(-1)).toMatchObject({
        action: "push",
        remote: "origin",
        tag: "release/v1",
      });
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("offers remote sources to New Branch and creates without checking out when asked", async () => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    h.snapshot = { ...refsSnapshot([]), remoteBranches: [ref("origin/release")] };
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);

    try {
      await act(async () =>
        root.render(
          <GitManagerToolbar
            projectRef={currentProject}
            mainCheckoutCwd="/opaque/main"
            selectedWorktreeCwd="/opaque/main"
            worktrees={worktrees}
            catalogPending={false}
            catalogError={null}
            repositoryUnavailable={null}
            branchSyncDisabledReason={null}
            stashMergeDisabledReason={null}
            tagDisabledReason={null}
            onSelectedWorktreeChange={() => undefined}
          />,
        ),
      );
      const props = h.branchDialogProps.at(-1)!;
      expect(props.remoteRefs).toEqual([ref("origin/release")]);
      await act(async () =>
        (props.onSubmit as (submission: unknown) => Promise<void>)({
          kind: "create",
          name: "feature/later",
          startPoint: "refs/remotes/origin/release",
          checkout: false,
        }),
      );
      expect(h.runOperation).toHaveBeenCalledWith(
        expect.anything(),
        expect.objectContaining({
          input: expect.objectContaining({
            _tag: "branch-create",
            name: "feature/later",
            startPoint: "refs/remotes/origin/release",
            checkout: false,
          }),
        }),
        expect.any(Function),
      );
    } finally {
      await act(async () => root.unmount());
      container.remove();
      (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
    }
  });

  it("treats an interrupted clone's placeholder HEAD as a repository without commits", () => {
    h.snapshot = {
      ...refsSnapshot(),
      headRef: null,
      detachedSha: null,
      defaultBranch: null,
      localBranches: [],
      remoteBranches: [],
    };
    const placeholder = renderToolbar();
    expect(placeholder).toContain("No commits yet");
    expect(placeholder).not.toContain("Detached HEAD");
    expect(placeholder).toContain("Fetch origin");
    expect(placeholder).not.toContain("Publish branch");

    h.snapshot = {
      ...refsSnapshot(),
      headRef: null,
      detachedSha: "b".repeat(40),
      localBranches: [ref("main", { isDefault: true })],
    };
    const detached = renderToolbar();
    expect(detached).toContain("Detached HEAD");
    expect(detached).not.toContain("No commits yet");
  });

  it("uses the outer panel's unavailable state ahead of cached branch and sync data", () => {
    h.snapshot = refsSnapshot();
    const reason =
      "Git can't read this repository. Check its .git folder, for example a damaged HEAD or config file.";
    const container = document.createElement("div");
    container.innerHTML = renderToolbar({
      repositoryUnavailable: "unreadable",
      branchSyncDisabledReason: reason,
      stashMergeDisabledReason: reason,
      tagDisabledReason: reason,
    });
    const branch = container.querySelector<HTMLButtonElement>('[aria-label="Choose branch"]');
    expect(branch?.textContent).toBe("No branch");
    expect(branch?.disabled).toBe(true);
    expect(branch?.title).toBe(
      "Git can't read this repository. Check its .git folder, for example a damaged HEAD or config file.",
    );
    expect(container.textContent).toContain("Sync unavailable");
    expect(container.textContent).not.toContain("Detached HEAD");
    expect(container.textContent).not.toContain("Fetch origin");
    expect(container.querySelector<HTMLButtonElement>('[aria-label="Worktree"]')?.disabled).toBe(
      false,
    );
  });

  it("does not advertise local tags as pending pushes without remote tag state", () => {
    h.snapshot = refsSnapshot([ref("already-published")]);

    const markup = renderToolbar();

    expect(markup).toContain("Fetch origin");
    expect(markup).not.toContain('aria-label="1 ahead"');
  });

  it("disables branch sync operations with their reason while tag and worktree controls remain", () => {
    const reason = "This environment does not support Git Manager branch and sync operations.";
    h.snapshot = refsSnapshot([ref("release/v1")]);

    const markup = renderToolbar({ branchSyncDisabledReason: reason });

    expect(markup).toContain(`title="${reason}"`);
    expect(markup).toContain(reason);
    expect(markup).toContain('aria-label="Choose branch"');
    expect(markup).toContain('aria-label="Worktree"');
    expect(markup).toContain("Tags…");
  });

  it("disables tag operations with their reason while branch sync remains available", () => {
    const reason = "This environment does not support Git Manager tag operations.";
    h.snapshot = refsSnapshot([ref("release/v1")]);

    const markup = renderToolbar({ tagDisabledReason: reason });

    expect(markup).toContain(`title="${reason}"`);
    expect(markup).toContain(reason);
    expect(markup).toContain('aria-label="Choose branch"');
    expect(markup).toContain("Fetch origin");
  });

  it("renders tag menu reasons at the smallest app text size", () => {
    const reason = "This environment does not support Git Manager tag operations.";
    h.snapshot = refsSnapshot([ref("release/v1")]);
    const disabled = renderToolbar({ tagDisabledReason: reason });
    h.snapshot = refsSnapshot();
    const empty = renderToolbar();

    for (const markup of [disabled, empty]) expect(markup).not.toMatch(/text-\[\d+px\]/);
    expect(disabled).toContain(`<span class="text-xs text-muted-foreground">${reason}</span>`);
    expect(empty).toContain('<span class="text-xs text-muted-foreground">No local tags.</span>');
  });

  it("skips the live signal subscription without disabling explicit repository reads", () => {
    h.snapshot = refsSnapshot();

    const markup = renderToolbar();

    expect(h.signalAtom).not.toHaveBeenCalled();
    expect(h.refsAtom).toHaveBeenCalledOnce();
    expect(markup).toContain('aria-label="Choose branch"');
    expect(markup).toContain("Fetch origin");
  });
});
