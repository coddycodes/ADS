// @vitest-environment happy-dom

import {
  DEFAULT_SERVER_SETTINGS,
  EnvironmentId,
  ProjectId,
  ProviderInstanceId,
  ThreadId,
  WorktreeKey,
  WorktreeRepositoryKey,
  type ServerConfig,
  type VcsWorktreeCatalogSnapshot,
  type VcsWorktreeDescriptor,
} from "@coddy/contracts";
import { createModelSelection } from "@coddy/shared/model";
import { AsyncResult } from "effect/reactivity";
import { act, cloneElement, type ReactElement, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { SidebarProjectSnapshot } from "../sidebarProjectGrouping";

const testState = vi.hoisted(() => ({
  catalogs: new Map<string, unknown>(),
  catalogSubscriptions: [] as Array<unknown>,
  focusRefreshProjects: [] as Array<unknown>,
  commandCalls: [] as Array<{ label: string; input: unknown }>,
  commandHandlers: new Map<string, (input: unknown) => Promise<unknown>>(),
  navigate: vi.fn(),
  toastAdd: vi.fn(),
}));

vi.mock("../state/worktrees", () => ({
  worktreeEnvironment: {
    catalog: (target: { environmentId: string; input: { projectId: string } }) => {
      testState.catalogSubscriptions.push(target);
      return { key: `${target.environmentId}:${target.input.projectId}` };
    },
    updatePolicy: { label: "worktree.updatePolicy" },
    addOne: { label: "worktree.addOne" },
    addAll: { label: "worktree.addAll" },
  },
  useWorktreeCatalogFocusRefresh: (projects: ReadonlyArray<unknown>) => {
    testState.focusRefreshProjects = [...projects];
  },
}));

vi.mock("../state/query", () => ({
  useEnvironmentQuery: (atom: { key: string } | null) => ({
    data: atom ? (testState.catalogs.get(atom.key) ?? null) : null,
    emission: AsyncResult.initial(false),
    error: null,
    isPending: false,
    refresh: vi.fn(),
  }),
}));

vi.mock("../state/use-atom-command", () => ({
  useAtomCommand: (command: { label: string }) => async (input: unknown) => {
    testState.commandCalls.push({ label: command.label, input });
    return (
      testState.commandHandlers.get(command.label)?.(input) ??
      Promise.resolve(AsyncResult.success(undefined))
    );
  },
}));

vi.mock("./ui/tooltip", () => ({
  Tooltip: ({ children }: { children: ReactNode }) => <>{children}</>,
  TooltipTrigger: ({ render, children }: { render: ReactElement; children?: ReactNode }) =>
    children === undefined ? render : cloneElement(render, undefined, children),
  TooltipPopup: ({ children }: { children: ReactNode }) => (
    <span data-mock="tooltip-popup">{children}</span>
  ),
}));

vi.mock("./ui/button", () => ({
  Button: ({ children, ...props }: React.ComponentProps<"button">) => (
    <button {...props}>{children}</button>
  ),
}));

vi.mock("./ui/toast", () => ({
  toastManager: { add: testState.toastAdd },
  stackedThreadToast: (toast: unknown) => toast,
}));

import { WorktreeDiscoverySection } from "./WorktreeDiscoverySection";

const ENVIRONMENT_ID = EnvironmentId.make("environment-main");
const PROJECT_ID = ProjectId.make("project-main");
const REMOTE_ENVIRONMENT_ID = EnvironmentId.make("environment-remote");
const REMOTE_PROJECT_ID = ProjectId.make("project-remote");
const LOCAL_ACCESSIBLE_SCOPE = `This device (environment ${ENVIRONMENT_ID}, project ${PROJECT_ID})`;
const REMOTE_ACCESSIBLE_SCOPE = `Remote Box (environment ${REMOTE_ENVIRONMENT_ID}, project ${REMOTE_PROJECT_ID})`;

function candidate(path: string, branch: string): VcsWorktreeDescriptor {
  return {
    worktreeKey: WorktreeKey.make(`key:${path}`),
    path,
    branch,
    head: "abcdef0123456789",
    isPrimary: false,
    isBare: false,
    locked: false,
    registrationState: "registered",
    directoryState: "present",
    adoptionState: "none",
    eligibleForAdoption: true,
  };
}

function snapshot(
  candidates: ReadonlyArray<VcsWorktreeDescriptor>,
  generation = 42,
): VcsWorktreeCatalogSnapshot {
  return {
    repositoryKey: WorktreeRepositoryKey.make("repository-main"),
    generation,
    authoritative: true,
    observedAt: "2026-08-09T12:00:00.000Z",
    scanStatus: { _tag: "ready" },
    worktrees: [...candidates],
    adoptedWorkspaces: [],
  };
}

function project(
  visibility: "hidden" | "shown" = "hidden",
  initialPromptDismissedAt: string | null = null,
  baselinePaths: ReadonlyArray<string> = [],
): SidebarProjectSnapshot {
  const member = {
    id: PROJECT_ID,
    title: "Repo",
    workspaceRoot: "/repo",
    repositoryIdentity: null,
    defaultModelSelection: createModelSelection(ProviderInstanceId.make("codex"), "gpt-5"),
    scripts: [],
    worktreeDiscovery: { visibility, initialPromptDismissedAt, baselinePaths },
    createdAt: "2026-08-09T00:00:00.000Z",
    updatedAt: "2026-08-09T00:00:00.000Z",
    environmentId: ENVIRONMENT_ID,
    physicalProjectKey: "environment-main:/repo",
    environmentLabel: "This device",
  } as const;
  return {
    ...member,
    projectKey: "repository-main",
    displayName: "Repo",
    groupedProjectCount: 1,
    environmentPresence: "local-only",
    allRemoteMembersAreDesktopLocal: false,
    memberProjects: [member],
    memberProjectRefs: [{ environmentId: ENVIRONMENT_ID, projectId: PROJECT_ID }],
    remoteEnvironmentLabels: [],
  } as SidebarProjectSnapshot;
}

function serverConfigs(supported: boolean): ReadonlyMap<EnvironmentId, ServerConfig> {
  return new Map([
    [
      ENVIRONMENT_ID,
      {
        environment: { capabilities: { worktreeCatalog: supported } },
        settings: DEFAULT_SERVER_SETTINGS,
        providers: [],
      } as unknown as ServerConfig,
    ],
  ]);
}

function groupedProject(
  localVisibility: "hidden" | "shown",
  remoteVisibility: "hidden" | "shown",
): SidebarProjectSnapshot {
  const localProject = project(localVisibility);
  const localMember = localProject.memberProjects[0]!;
  const remoteMember = {
    ...localMember,
    id: REMOTE_PROJECT_ID,
    workspaceRoot: "R:\\repo",
    worktreeDiscovery: {
      ...localMember.worktreeDiscovery,
      visibility: remoteVisibility,
    },
    environmentId: REMOTE_ENVIRONMENT_ID,
    physicalProjectKey: "environment-remote:R:\\repo",
    environmentLabel: "Remote Box",
  };
  return {
    ...localProject,
    groupedProjectCount: 2,
    environmentPresence: "mixed",
    memberProjects: [localMember, remoteMember],
    memberProjectRefs: [
      { environmentId: ENVIRONMENT_ID, projectId: PROJECT_ID },
      { environmentId: REMOTE_ENVIRONMENT_ID, projectId: REMOTE_PROJECT_ID },
    ],
    remoteEnvironmentLabels: ["Remote Box"],
  };
}

function groupedServerConfigs(): ReadonlyMap<EnvironmentId, ServerConfig> {
  const config = {
    environment: { capabilities: { worktreeCatalog: true } },
    settings: DEFAULT_SERVER_SETTINGS,
    providers: [],
  } as unknown as ServerConfig;
  return new Map([
    [ENVIRONMENT_ID, config],
    [REMOTE_ENVIRONMENT_ID, config],
  ]);
}

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];

async function mount(element: ReactElement): Promise<HTMLDivElement> {
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedTrees.push({ container, root });
  await act(async () => root.render(element));
  return container;
}

async function unmountLastMountedTree(): Promise<void> {
  const mounted = mountedTrees.pop();
  if (!mounted) throw new Error("No mounted tree to unmount");
  await act(async () => mounted.root.unmount());
  mounted.container.remove();
}

function button(name: string): HTMLButtonElement {
  const result = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (entry) => entry.getAttribute("aria-label") === name || entry.textContent?.trim() === name,
  );
  if (!result) throw new Error(`Missing button: ${name}`);
  return result;
}

beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  testState.catalogs.clear();
  testState.catalogSubscriptions = [];
  testState.focusRefreshProjects = [];
  testState.commandCalls = [];
  testState.commandHandlers.clear();
  testState.navigate.mockReset();
  testState.toastAdd.mockReset();
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  vi.unstubAllGlobals();
});

describe("WorktreeDiscoverySection", () => {
  it("reports the hidden candidate count while mounted and clears it on unmount", async () => {
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([
        candidate("/worktrees/one", "feature/one"),
        candidate("/worktrees/two", "feature/two"),
      ]),
    );
    const onHiddenCountChange = vi.fn();
    await mount(
      <WorktreeDiscoverySection
        project={project("hidden")}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
        onHiddenCountChange={onHiddenCountChange}
      />,
    );
    expect(onHiddenCountChange).toHaveBeenLastCalledWith(2);

    await unmountLastMountedTree();
    expect(onHiddenCountChange).toHaveBeenLastCalledWith(null);
  });

  it.each([
    { remoteLoaded: false, expected: null },
    { remoteLoaded: true, expected: 3 },
  ])(
    "reports a grouped count only when all members are known: %j",
    async ({ remoteLoaded, expected }) => {
      testState.catalogs.set(
        `${ENVIRONMENT_ID}:${PROJECT_ID}`,
        snapshot([candidate("/wt/one", "one"), candidate("/wt/two", "two")]),
      );
      if (remoteLoaded) {
        testState.catalogs.set(
          `${REMOTE_ENVIRONMENT_ID}:${REMOTE_PROJECT_ID}`,
          snapshot([candidate("/wt/remote", "remote")]),
        );
      }
      const onHiddenCountChange = vi.fn();
      await mount(
        <WorktreeDiscoverySection
          project={groupedProject("hidden", "hidden")}
          serverConfigs={groupedServerConfigs()}
          onNavigateToThread={testState.navigate}
          onHiddenCountChange={onHiddenCountChange}
        />,
      );
      expect(onHiddenCountChange).toHaveBeenLastCalledWith(expected);
      if (!remoteLoaded)
        expect(onHiddenCountChange.mock.calls.every(([count]) => count === null)).toBe(true);
      await unmountLastMountedTree();
      expect(onHiddenCountChange).toHaveBeenLastCalledWith(null);
    },
  );

  it("counts a known shown member as zero alongside a hidden member", async () => {
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([candidate("/wt/local", "local")]),
    );
    testState.catalogs.set(
      `${REMOTE_ENVIRONMENT_ID}:${REMOTE_PROJECT_ID}`,
      snapshot([candidate("/wt/remote", "remote")]),
    );
    const onHiddenCountChange = vi.fn();
    await mount(
      <WorktreeDiscoverySection
        project={groupedProject("hidden", "shown")}
        serverConfigs={groupedServerConfigs()}
        onNavigateToThread={testState.navigate}
        onHiddenCountChange={onHiddenCountChange}
      />,
    );
    expect(onHiddenCountChange).toHaveBeenLastCalledWith(1);
  });

  it("reports zero hidden worktrees while discovery is shown", async () => {
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([candidate("/worktrees/one", "feature/one")]),
    );
    const onHiddenCountChange = vi.fn();
    await mount(
      <WorktreeDiscoverySection
        project={project("shown")}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
        onHiddenCountChange={onHiddenCountChange}
      />,
    );
    expect(onHiddenCountChange).toHaveBeenLastCalledWith(0);
  });

  it("keeps identical labels, candidate names, and host paths unique by physical scope", async () => {
    const sharedEnvironmentLabel = "Shared host";
    const sharedPath = "/worktrees/shared";
    const sharedBranch = "feature/shared";
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([candidate(sharedPath, sharedBranch)]),
    );
    testState.catalogs.set(
      `${REMOTE_ENVIRONMENT_ID}:${REMOTE_PROJECT_ID}`,
      snapshot([candidate(sharedPath, sharedBranch)]),
    );
    const withSharedLabels = (snapshot: SidebarProjectSnapshot): SidebarProjectSnapshot => ({
      ...snapshot,
      memberProjects: snapshot.memberProjects.map((member) => ({
        ...member,
        environmentLabel: sharedEnvironmentLabel,
      })),
      remoteEnvironmentLabels: [sharedEnvironmentLabel],
    });

    await mount(
      <WorktreeDiscoverySection
        project={withSharedLabels(groupedProject("hidden", "hidden"))}
        serverConfigs={groupedServerConfigs()}
        onNavigateToThread={testState.navigate}
      />,
    );

    const addNames = Array.from(document.querySelectorAll<HTMLButtonElement>("button"))
      .map((element) => element.getAttribute("aria-label"))
      .filter((name): name is string => name?.startsWith("Add feature/shared from") === true);
    expect(addNames).toEqual([
      `Add feature/shared from Shared host (environment ${ENVIRONMENT_ID}, project ${PROJECT_ID}) at ${sharedPath} to Coddy Studio`,
      `Add feature/shared from Shared host (environment ${REMOTE_ENVIRONMENT_ID}, project ${REMOTE_PROJECT_ID}) at ${sharedPath} to Coddy Studio`,
    ]);
    expect(new Set(addNames).size).toBe(2);

    const pathTriggerNames = Array.from(document.querySelectorAll<HTMLElement>("[aria-label]"))
      .map((element) => element.getAttribute("aria-label"))
      .filter((name): name is string => name?.startsWith("Full worktree path for") === true);
    expect(pathTriggerNames).toEqual([
      `Full worktree path for feature/shared in Shared host (environment ${ENVIRONMENT_ID}, project ${PROJECT_ID}): ${sharedPath}`,
      `Full worktree path for feature/shared in Shared host (environment ${REMOTE_ENVIRONMENT_ID}, project ${REMOTE_PROJECT_ID}): ${sharedPath}`,
    ]);
    expect(new Set(pathTriggerNames).size).toBe(2);
    expect(document.body.textContent).toContain(sharedEnvironmentLabel);
    expect(document.body.textContent).not.toContain(ENVIRONMENT_ID);
    expect(document.body.textContent).not.toContain(REMOTE_ENVIRONMENT_ID);
    expect(
      Array.from(document.querySelectorAll<HTMLElement>("[data-mock='tooltip-popup']")).filter(
        (element) => element.textContent === sharedPath,
      ),
    ).toHaveLength(2);

    await unmountLastMountedTree();
    await mount(
      <WorktreeDiscoverySection
        project={withSharedLabels(groupedProject("shown", "shown"))}
        serverConfigs={groupedServerConfigs()}
        onNavigateToThread={testState.navigate}
      />,
    );

    const shownNames = Array.from(document.querySelectorAll<HTMLButtonElement>("button"))
      .map((element) => element.getAttribute("aria-label"))
      .filter(
        (name): name is string =>
          name?.startsWith("Add discovered worktree feature/shared") === true,
      );
    expect(shownNames).toEqual([
      `Add discovered worktree feature/shared from Shared host (environment ${ENVIRONMENT_ID}, project ${PROJECT_ID}) at ${sharedPath} to Coddy Studio`,
      `Add discovered worktree feature/shared from Shared host (environment ${REMOTE_ENVIRONMENT_ID}, project ${REMOTE_PROJECT_ID}) at ${sharedPath} to Coddy Studio`,
    ]);
    expect(new Set(shownNames).size).toBe(2);
  });

  it("uniquely names same-branch actions and discloses exact paths on focus and hover", async () => {
    const localPath = '/worktrees/<script>alert("x")</script>/same';
    const remotePath = "R:\\worktrees\\same";
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([candidate(localPath, "feature/shared")]),
    );
    testState.catalogs.set(
      `${REMOTE_ENVIRONMENT_ID}:${REMOTE_PROJECT_ID}`,
      snapshot([candidate(remotePath, "feature/shared")]),
    );

    await mount(
      <WorktreeDiscoverySection
        project={groupedProject("hidden", "shown")}
        serverConfigs={groupedServerConfigs()}
        onNavigateToThread={testState.navigate}
      />,
    );

    expect(
      button(`Add feature/shared from ${LOCAL_ACCESSIBLE_SCOPE} at ${localPath} to Coddy Studio`),
    ).toBeDefined();
    expect(
      button(
        `Add discovered worktree feature/shared from ${REMOTE_ACCESSIBLE_SCOPE} at ${remotePath} to Coddy Studio`,
      ),
    ).toBeDefined();

    const localPathTrigger = Array.from(
      document.querySelectorAll<HTMLElement>("[aria-label]"),
    ).find(
      (element) =>
        element.getAttribute("aria-label") ===
        `Full worktree path for feature/shared in ${LOCAL_ACCESSIBLE_SCOPE}: ${localPath}`,
    );
    expect(localPathTrigger).not.toBeNull();
    expect(localPathTrigger?.tabIndex).toBe(0);
    const tooltips = Array.from(
      document.querySelectorAll<HTMLElement>("[data-mock='tooltip-popup']"),
    ).map((element) => element.textContent);
    expect(tooltips).toContain(localPath);
    expect(tooltips).toContain(remotePath);
    expect(document.querySelector("script")).toBeNull();
  });

  it.each([
    ["initial card", "hidden"],
    ["shown rows", "shown"],
  ] as const)(
    "renders parent-once compact candidates with stable trailing actions in the %s",
    async (_mode, visibility) => {
      const hotfixPath = "/Users/admin/conductor/workspaces/pathfinder-hotfix";
      const reviewPath = "/Users/admin/conductor/workspaces/pathfinder-review";
      const alphaPath = "/Users/admin/orca/workspaces/alpha";
      const candidates = [
        candidate(reviewPath, "hotfix/PFS-1817"),
        candidate(alphaPath, "alpha"),
        candidate(hotfixPath, "hotfix/PFS-1817"),
      ];
      testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot(candidates));

      await mount(
        <WorktreeDiscoverySection
          project={project(visibility)}
          serverConfigs={serverConfigs(true)}
          onNavigateToThread={testState.navigate}
        />,
      );

      const parentRows = Array.from(
        document.querySelectorAll<HTMLElement>("[data-worktree-parent-directory]"),
      );
      expect(parentRows.map((row) => row.textContent)).toEqual([
        "/Users/admin/conductor/workspaces",
        "/Users/admin/orca/workspaces",
      ]);

      const candidateRows = Array.from(
        document.querySelectorAll<HTMLElement>("[data-worktree-candidate-row]"),
      );
      expect(candidateRows).toHaveLength(3);
      const expectedCopy = [
        ["hotfix/PFS-1817pathfinder-hotfix", hotfixPath],
        ["hotfix/PFS-1817pathfinder-review", reviewPath],
        ["alpha", alphaPath],
      ] as const;

      for (const [index, [visibleCopy, path]] of expectedCopy.entries()) {
        const candidateRow = candidateRows[index]!;
        const copy = candidateRow.querySelector<HTMLElement>("[data-worktree-candidate-copy]");
        expect(copy?.textContent).toBe(visibleCopy);
        expect(copy?.textContent).not.toContain(path);
        expect(copy?.className).toContain("min-w-0");
        expect(copy?.className).toContain("overflow-hidden");
        expect(copy?.tabIndex).toBe(0);
        expect(copy?.getAttribute("aria-label")).toBe(
          `Full worktree path for ${index < 2 ? "hotfix/PFS-1817" : "alpha"} in ${LOCAL_ACCESSIBLE_SCOPE}: ${path}`,
        );
        expect(copy?.querySelector<HTMLElement>("span")?.className.split(/\s+/)).toEqual(
          expect.arrayContaining(["block", "max-w-full", "truncate"]),
        );
        expect(
          Array.from(candidateRow.querySelectorAll<HTMLElement>("[data-mock='tooltip-popup']"))
            .map((popup) => popup.textContent)
            .filter((text) => text === path),
        ).toHaveLength(1);

        const addAction = candidateRow.lastElementChild as HTMLButtonElement | null;
        expect(addAction?.getAttribute("data-worktree-add-action")).toBe("true");
        expect(addAction?.className).toContain("shrink-0");
        expect(addAction?.getAttribute("aria-label")).toContain(` at ${path} to Coddy Studio`);
      }
    },
  );

  it("adds one candidate and navigates to the returned scoped workspace", async () => {
    const discovered = candidate("/worktrees/one", "feature/one");
    testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot([discovered]));
    testState.commandHandlers.set("worktree.addOne", async () =>
      AsyncResult.success({ threadId: ThreadId.make("thread-adopted"), disposition: "created" }),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    await act(async () =>
      button(`Add feature/one from ${LOCAL_ACCESSIBLE_SCOPE} at /worktrees/one to Coddy Studio`).click(),
    );

    expect(testState.commandCalls).toContainEqual({
      label: "worktree.addOne",
      input: {
        environmentId: ENVIRONMENT_ID,
        input: expect.objectContaining({
          projectId: PROJECT_ID,
          worktreeKey: discovered.worktreeKey,
          expectedGeneration: 42,
        }),
      },
    });
    expect(testState.navigate).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      threadId: ThreadId.make("thread-adopted"),
    });
  });

  it("keeps the current route while add-all reports pending and mixed results", async () => {
    const candidates = [
      candidate("/worktrees/one", "feature/one"),
      candidate("/worktrees/two", "feature/two"),
    ];
    testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot(candidates));
    let resolveAddAll!: (value: unknown) => void;
    testState.commandHandlers.set(
      "worktree.addAll",
      () => new Promise((resolve) => (resolveAddAll = resolve)),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    await act(async () => {
      button("Add all discovered worktrees").click();
      await Promise.resolve();
    });
    expect(document.body.textContent).toContain("Adding 2 discovered worktrees…");

    await act(async () =>
      resolveAddAll(
        AsyncResult.success({
          results: [
            {
              _tag: "Success",
              worktreeKey: candidates[0]!.worktreeKey,
              result: { threadId: ThreadId.make("thread-one"), disposition: "created" },
            },
            {
              _tag: "Failure",
              worktreeKey: candidates[1]!.worktreeKey,
              error: new Error("stale"),
            },
          ],
        }),
      ),
    );

    expect(testState.navigate).not.toHaveBeenCalled();
    expect(testState.toastAdd).toHaveBeenCalledWith({
      type: "warning",
      title: "Added 1 of 2 discovered worktrees",
      description: "1 worktree could not be added.",
    });
  });

  it("excludes an add-one key from add-all progress and summary", async () => {
    const candidates = [
      candidate("/worktrees/one", "feature/one"),
      candidate("/worktrees/two", "feature/two"),
    ];
    testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot(candidates));
    let resolveAddOne!: (value: unknown) => void;
    let resolveAddAll!: (value: unknown) => void;
    testState.commandHandlers.set(
      "worktree.addOne",
      () => new Promise((resolve) => (resolveAddOne = resolve)),
    );
    testState.commandHandlers.set(
      "worktree.addAll",
      () => new Promise((resolve) => (resolveAddAll = resolve)),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    await act(async () => {
      button(`Add feature/one from ${LOCAL_ACCESSIBLE_SCOPE} at /worktrees/one to Coddy Studio`).click();
      await Promise.resolve();
    });
    await act(async () => {
      button("Add all discovered worktrees").click();
      await Promise.resolve();
    });

    expect(document.body.textContent).toContain("Adding 1 discovered worktree…");
    const addAllCall = testState.commandCalls.find((entry) => entry.label === "worktree.addAll");
    expect(addAllCall?.input).toEqual({
      environmentId: ENVIRONMENT_ID,
      input: {
        candidates: [
          expect.objectContaining({
            worktreeKey: candidates[1]!.worktreeKey,
            expectedGeneration: 42,
          }),
        ],
      },
    });
    expect(testState.navigate).not.toHaveBeenCalled();

    await act(async () =>
      resolveAddAll(
        AsyncResult.success({
          results: [
            {
              _tag: "Success",
              worktreeKey: candidates[1]!.worktreeKey,
              result: { threadId: ThreadId.make("thread-two"), disposition: "created" },
            },
          ],
        }),
      ),
    );
    expect(testState.toastAdd).toHaveBeenCalledWith({
      type: "success",
      title: "Added 1 discovered worktree",
      description: "All discovered worktrees were added to Coddy Studio.",
    });

    await act(async () =>
      resolveAddOne(
        AsyncResult.success({
          threadId: ThreadId.make("thread-one"),
          disposition: "created",
        }),
      ),
    );
  });

  it("disables add-all when every candidate key already has an add-one pending", async () => {
    const discovered = candidate("/worktrees/only", "feature/only");
    testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot([discovered]));
    let resolveAddOne!: (value: unknown) => void;
    testState.commandHandlers.set(
      "worktree.addOne",
      () => new Promise((resolve) => (resolveAddOne = resolve)),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    await act(async () => {
      button(
        `Add feature/only from ${LOCAL_ACCESSIBLE_SCOPE} at /worktrees/only to Coddy Studio`,
      ).click();
      await Promise.resolve();
    });

    const addAllButton = button("Add all discovered worktrees");
    expect(addAllButton.disabled).toBe(true);
    addAllButton.click();
    expect(testState.commandCalls.some((entry) => entry.label === "worktree.addAll")).toBe(false);

    await act(async () =>
      resolveAddOne(
        AsyncResult.success({
          threadId: ThreadId.make("thread-only"),
          disposition: "created",
        }),
      ),
    );
  });

  it("ignores delayed adoption completion after the discovery subtree unmounts", async () => {
    const discovered = candidate("/worktrees/delayed", "feature/delayed");
    testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot([discovered]));
    let resolveAddOne!: (value: unknown) => void;
    testState.commandHandlers.set(
      "worktree.addOne",
      () => new Promise((resolve) => (resolveAddOne = resolve)),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    await act(async () => {
      button(
        `Add feature/delayed from ${LOCAL_ACCESSIBLE_SCOPE} at /worktrees/delayed to Coddy Studio`,
      ).click();
      await Promise.resolve();
    });
    await unmountLastMountedTree();
    await act(async () =>
      resolveAddOne(
        AsyncResult.success({
          threadId: ThreadId.make("thread-delayed"),
          disposition: "created",
        }),
      ),
    );

    expect(testState.navigate).not.toHaveBeenCalled();
    expect(testState.toastAdd).not.toHaveBeenCalled();
  });

  it("acknowledges the exact generation, collapses, and can expand the hidden line", async () => {
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([candidate("/worktrees/one", "feature/one")], 87),
    );
    testState.commandHandlers.set("worktree.updatePolicy", async () =>
      AsyncResult.success({
        visibility: "hidden",
        initialPromptDismissedAt: null,
        baselinePaths: [],
      }),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    await act(async () => button("Keep hidden").click());
    expect(testState.commandCalls).toContainEqual({
      label: "worktree.updatePolicy",
      input: {
        environmentId: ENVIRONMENT_ID,
        input: expect.objectContaining({
          projectId: PROJECT_ID,
          acknowledgeGeneration: 87,
          dismissInitialPrompt: true,
        }),
      },
    });
    expect(document.body.textContent).toContain("Hiding 1 discovered worktree");

    await act(async () => button("Hiding 1 discovered worktree").click());
    expect(document.body.textContent).toContain("Discovered worktrees");
    expect(document.body.textContent).toContain("/worktrees/one");
  });

  it("resurfaces only new candidates after acknowledgement and limits add-all to that set", async () => {
    const acknowledged = candidate("/worktrees/acknowledged", "feature/acknowledged");
    const newlyDiscovered = candidate("/worktrees/new", "feature/new");
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([acknowledged, newlyDiscovered], 91),
    );
    testState.commandHandlers.set("worktree.addAll", async () =>
      AsyncResult.success({
        results: [
          {
            _tag: "Success",
            worktreeKey: newlyDiscovered.worktreeKey,
            result: { threadId: ThreadId.make("thread-new"), disposition: "created" },
          },
        ],
      }),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project("hidden", "2026-08-09T12:00:00.000Z", [acknowledged.path])}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    expect(document.body.textContent).toContain("/worktrees/new");
    expect(document.body.textContent).not.toContain("/worktrees/acknowledged");
    await act(async () => button("Add all discovered worktrees").click());

    const call = testState.commandCalls.find((entry) => entry.label === "worktree.addAll");
    expect(call?.input).toEqual({
      environmentId: ENVIRONMENT_ID,
      input: {
        candidates: [
          expect.objectContaining({
            worktreeKey: newlyDiscovered.worktreeKey,
            expectedGeneration: 91,
          }),
        ],
      },
    });
  });

  it("renders shown candidates as discovered rows whose selection adopts them", async () => {
    const discovered = candidate("/worktrees/shown", "feature/shown");
    testState.catalogs.set(`${ENVIRONMENT_ID}:${PROJECT_ID}`, snapshot([discovered]));
    testState.commandHandlers.set("worktree.addOne", async () =>
      AsyncResult.success({ threadId: ThreadId.make("thread-shown"), disposition: "created" }),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project("shown")}
        serverConfigs={serverConfigs(true)}
        onNavigateToThread={testState.navigate}
      />,
    );

    expect(document.body.textContent).toContain("Discovered");
    expect(document.body.textContent).toContain("/worktrees/shown");
    await act(async () =>
      button(
        `Add discovered worktree feature/shown from ${LOCAL_ACCESSIBLE_SCOPE} at /worktrees/shown to Coddy Studio`,
      ).click(),
    );

    expect(testState.commandCalls.some((call) => call.label === "worktree.addOne")).toBe(true);
    expect(testState.navigate).toHaveBeenCalledWith({
      environmentId: ENVIRONMENT_ID,
      threadId: ThreadId.make("thread-shown"),
    });
  });

  it("renders no controls and creates no catalog subscription for unsupported environments", async () => {
    testState.catalogs.set(
      `${ENVIRONMENT_ID}:${PROJECT_ID}`,
      snapshot([candidate("/worktrees/unsupported", "feature/unsupported")]),
    );
    await mount(
      <WorktreeDiscoverySection
        project={project()}
        serverConfigs={serverConfigs(false)}
        onNavigateToThread={testState.navigate}
      />,
    );

    expect(testState.catalogSubscriptions).toEqual([]);
    expect(testState.focusRefreshProjects).toEqual([]);
    expect(document.body.textContent).not.toContain("Discovered worktrees");
    expect(document.body.textContent).not.toContain("feature/unsupported");
  });
});
