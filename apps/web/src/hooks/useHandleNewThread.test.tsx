// @vitest-environment happy-dom

import {
  scopedProjectKey,
  scopeProjectRef,
  scopeThreadRef,
} from "@coddy/client-runtime/environment";
import {
  DEFAULT_MODEL_BY_PROVIDER,
  DEFAULT_SERVER_SETTINGS,
  defaultInstanceIdForDriver,
  EnvironmentId,
  ProjectId,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
  type ProviderOptionDescriptor,
  type ScopedProjectRef,
  type ServerConfig,
  type ServerProvider,
  type ServerProviderModel,
} from "@coddy/contracts";
import { createModelSelection } from "@coddy/shared/model";
import { act, type ReactElement, useState, useSyncExternalStore } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

const testState = vi.hoisted(() => ({
  projects: [] as Array<Record<string, unknown>>,
  serverConfigs: new Map<string, Pick<ServerConfig, "providers" | "settings">>(),
  serverConfigSubscribers: new Set<() => void>(),
  clientSettings: { groupProjectsBy: "folder" },
  routeParams: {} as Record<string, string | undefined>,
  router: {
    state: { matches: [{ params: {} as Record<string, string | undefined> }] },
    navigate: vi.fn(),
  },
  activeThread: null as Record<string, unknown> | null,
  shellExists: false,
  projectOrder: [] as string[],
  logicalProjectKey: "logical:project",
}));

vi.mock("@tanstack/react-router", () => ({
  useRouter: () => testState.router,
  useParams: (options: { select: (params: Record<string, string | undefined>) => unknown }) =>
    options.select(testState.routeParams),
}));

vi.mock("../state/entities", () => ({
  useProjects: () => testState.projects,
  useServerConfigs: () =>
    useSyncExternalStore(
      (onStoreChange) => {
        testState.serverConfigSubscribers.add(onStoreChange);
        return () => testState.serverConfigSubscribers.delete(onStoreChange);
      },
      () => testState.serverConfigs,
    ),
  useThread: (threadRef: unknown) => (threadRef ? testState.activeThread : null),
  readThreadShell: () => (testState.shellExists ? { status: "ready" } : null),
}));

vi.mock("../logicalProject", async (importOriginal) => ({
  deriveLogicalProjectKeyFromSettings: () => testState.logicalProjectKey,
  projectDraftFallbackKeys: (await importOriginal<typeof import("../logicalProject")>())
    .projectDraftFallbackKeys,
  getProjectOrderKey: (project: { environmentId: string; id: string }) =>
    `${project.environmentId}:${project.id}`,
  selectProjectGroupingSettings: (settings: unknown) => settings,
}));

vi.mock("../uiStateStore", () => ({
  legacyProjectCwdPreferenceKey: (workspaceRoot: string) => `legacy:${workspaceRoot}`,
  useUiStateStore: (selector: (store: { projectOrder: string[] }) => unknown) =>
    selector({ projectOrder: testState.projectOrder }),
}));

vi.mock("./useSettings", () => ({
  useClientSettings: (selector: (settings: Record<string, unknown>) => unknown) =>
    selector(testState.clientSettings),
}));

import { type DraftThreadEnvMode, useComposerDraftStore } from "../composerDraftStore";
import { useHandleNewThread, useNewThreadHandler } from "./useHandleNewThread";

const environmentId = EnvironmentId.make("local-test");
const projectId = ProjectId.make("project-test");
const projectRef = scopeProjectRef(environmentId, projectId);
const codexDriver = ProviderDriverKind.make("codex");
const claudeDriver = ProviderDriverKind.make("claudeAgent");
const codexInstanceId = defaultInstanceIdForDriver(codexDriver);
const claudeWorkInstanceId = ProviderInstanceId.make("claude_work");

const reasoningEffortDescriptor: ProviderOptionDescriptor = {
  id: "reasoningEffort",
  label: "Reasoning",
  type: "select",
  options: [
    { id: "low", label: "Low" },
    { id: "medium", label: "Medium", isDefault: true },
    { id: "high", label: "High" },
  ],
  currentValue: "medium",
};

const serviceTierDescriptor: ProviderOptionDescriptor = {
  id: "serviceTier",
  label: "Service tier",
  type: "select",
  options: [
    { id: "default", label: "Standard", isDefault: true },
    { id: "fast", label: "Fast" },
  ],
  currentValue: "default",
};

const claudeEffortDescriptor: ProviderOptionDescriptor = {
  id: "effort",
  label: "Effort",
  type: "select",
  options: [
    { id: "low", label: "Low", isDefault: true },
    { id: "high", label: "High" },
  ],
  currentValue: "low",
};

const claudePromptInjectedEffortDescriptor: ProviderOptionDescriptor = {
  ...claudeEffortDescriptor,
  options: [
    { id: "low", label: "Low", isDefault: true },
    { id: "high", label: "High" },
    { id: "ultrathink", label: "Ultrathink" },
  ],
  promptInjectedValues: ["ultrathink"],
};

const fastModeDescriptor: ProviderOptionDescriptor = {
  id: "fastMode",
  label: "Fast mode",
  type: "boolean",
  currentValue: false,
};

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

interface NewThreadOptions {
  branch?: string | null;
  worktreePath?: string | null;
  envMode?: DraftThreadEnvMode;
  startFromOrigin?: boolean;
}

const mountedTrees: MountedTree[] = [];

function setRoute(params: Record<string, string | undefined>): void {
  testState.routeParams = params;
  testState.router.state.matches = [{ params }];
}

function project(
  id: ProjectId = projectId,
  environment: EnvironmentId = environmentId,
  overrides: Record<string, unknown> = {},
) {
  return {
    id,
    environmentId: environment,
    workspaceRoot: `X:\\repos\\${id}`,
    defaultModelSelection: null,
    ...overrides,
  };
}

function providerModel(
  slug: string,
  descriptors: ReadonlyArray<ProviderOptionDescriptor>,
  options: { readonly isCustom?: boolean } = {},
): ServerProviderModel {
  return {
    slug,
    name: slug,
    isCustom: options.isCustom ?? false,
    capabilities: { optionDescriptors: [...descriptors] },
  };
}

function serverProvider(input: {
  readonly instanceId: ProviderInstanceId;
  readonly driver: ProviderDriverKind;
  readonly models: ReadonlyArray<ServerProviderModel>;
}): ServerProvider {
  return {
    instanceId: input.instanceId,
    driver: input.driver,
    enabled: true,
    installed: true,
    version: null,
    status: "ready",
    auth: { status: "authenticated" },
    checkedAt: "2026-07-20T00:00:00.000Z",
    models: [...input.models],
    slashCommands: [],
    skills: [],
    agents: [],
  };
}

function configureEnvironment(input: {
  readonly providers: ReadonlyArray<ServerProvider>;
  readonly settings?: Partial<typeof DEFAULT_SERVER_SETTINGS>;
}): void {
  const serverConfigs = new Map(testState.serverConfigs);
  serverConfigs.set(environmentId, {
    providers: [...input.providers],
    settings: {
      ...DEFAULT_SERVER_SETTINGS,
      ...input.settings,
    },
  });
  testState.serverConfigs = serverConfigs;
  for (const subscriber of testState.serverConfigSubscribers) subscriber();
}

function createdDraftModelSelection() {
  const draft = useComposerDraftStore.getState().getDraftSessionByProjectRef(projectRef);
  expect(draft).not.toBeNull();
  const composerDraft = useComposerDraftStore.getState().getComposerDraft(draft!.draftId);
  expect(composerDraft).not.toBeNull();
  const activeProvider = composerDraft!.activeProvider;
  expect(activeProvider).not.toBeNull();
  return composerDraft!.modelSelectionByProvider[activeProvider!];
}

function NewThreadHarness({
  target = projectRef,
  options,
}: {
  target?: ScopedProjectRef;
  options?: NewThreadOptions;
}) {
  const handleNewThread = useNewThreadHandler();
  const [status, setStatus] = useState("idle");
  return (
    <>
      <button
        type="button"
        onClick={() => {
          setStatus("running");
          void handleNewThread(target, options).then(() => setStatus("done"));
        }}
      >
        New thread
      </button>
      <output data-testid="status">{status}</output>
    </>
  );
}

function HandleSummary() {
  const state = useHandleNewThread();
  return (
    <output
      data-testid="summary"
      data-default-project={state.defaultProjectRef?.projectId ?? ""}
      data-route-thread={state.routeThreadRef?.threadId ?? ""}
      data-active-thread={state.activeThread ? "yes" : "no"}
      data-active-draft={state.activeDraftThread?.threadId ?? ""}
      data-handler={typeof state.handleNewThread}
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

async function clickNewThread(): Promise<void> {
  const button = Array.from(document.querySelectorAll<HTMLButtonElement>("button")).find(
    (candidate) => candidate.textContent === "New thread",
  );
  expect(button).toBeDefined();
  await act(async () => {
    button!.click();
    await Promise.resolve();
    await Promise.resolve();
  });
  expect(document.querySelector('[data-testid="status"]')?.textContent).toBe("done");
}

interface ResettableStore {
  getInitialState: () => object;
  setState: (state: object, replace?: boolean) => void;
}

const resettableDraftStore = useComposerDraftStore as unknown as ResettableStore;
const pristineDraftState = { ...resettableDraftStore.getInitialState() };

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  resettableDraftStore.setState({ ...pristineDraftState }, true);
  testState.projects = [];
  testState.serverConfigs = new Map();
  testState.router.navigate.mockReset().mockResolvedValue(undefined);
  testState.activeThread = null;
  testState.shellExists = false;
  testState.projectOrder = [];
  testState.logicalProjectKey = "logical:project";
  vi.spyOn(console, "warn").mockImplementation(() => {});
  setRoute({});
});

afterEach(async () => {
  for (const mounted of mountedTrees.splice(0)) {
    await act(async () => mounted.root.unmount());
    mounted.container.remove();
  }
  document.body.replaceChildren();
  resettableDraftStore.setState({ ...pristineDraftState }, true);
  vi.restoreAllMocks();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("useNewThreadHandler", () => {
  it("creates and navigates to a fresh draft with default environment settings", async () => {
    await mount(<NewThreadHarness />);
    await clickNewThread();

    const draft = useComposerDraftStore.getState().getDraftSessionByProjectRef(projectRef);
    expect(draft).not.toBeNull();
    expect(draft).toMatchObject({
      environmentId,
      projectId,
      branch: null,
      worktreePath: null,
      envMode: DEFAULT_SERVER_SETTINGS.defaultThreadEnvMode,
    });
    expect(testState.router.navigate).toHaveBeenCalledWith({
      to: "/draft/$draftId",
      params: { draftId: draft!.draftId },
    });
    expect(createdDraftModelSelection()).toMatchObject({
      instanceId: codexInstanceId,
      model: DEFAULT_MODEL_BY_PROVIDER[codexDriver],
    });
  });

  it("seeds a normal new draft from configured defaults instead of a stale project snapshot", async () => {
    testState.projects = [
      project(projectId, environmentId, {
        defaultModelSelection: createModelSelection(codexInstanceId, "gpt-stale", [
          { id: "reasoningEffort", value: "low" },
          { id: "serviceTier", value: "default" },
        ]),
      }),
    ];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [
            providerModel("gpt-stale", [reasoningEffortDescriptor, serviceTierDescriptor]),
            providerModel("gpt-configured", [reasoningEffortDescriptor, serviceTierDescriptor]),
          ],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: {
            model: "gpt-configured",
            options: [
              { id: "reasoningEffort", value: "medium" },
              { id: "serviceTier", value: "fast" },
            ],
          },
        },
      },
    });
    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: codexInstanceId,
      model: "gpt-configured",
      options: [
        { id: "reasoningEffort", value: "medium" },
        { id: "serviceTier", value: "fast" },
      ],
    });
  });

  it("seeds a normal new draft from a Claude prompt-injected effort default", async () => {
    testState.projects = [project()];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: claudeWorkInstanceId,
          driver: claudeDriver,
          models: [providerModel("claude-sonnet-5", [claudePromptInjectedEffortDescriptor])],
        }),
      ],
      settings: {
        providerInstances: {
          [claudeWorkInstanceId]: { driver: claudeDriver },
        },
        providerSessionDefaults: {
          [claudeDriver]: {
            model: "claude-sonnet-5",
            options: [{ id: "effort", value: "ultrathink" }],
          },
        },
      },
    });
    useComposerDraftStore
      .getState()
      .setStickyModelSelection(createModelSelection(claudeWorkInstanceId, "claude-sonnet-5"));

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: claudeWorkInstanceId,
      model: "claude-sonnet-5",
      options: [{ id: "effort", value: "ultrathink" }],
    });
  });

  it("uses the newest saved Codex defaults only for the next chat", async () => {
    testState.projects = [project()];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [
            providerModel("gpt-initial", [reasoningEffortDescriptor, serviceTierDescriptor]),
          ],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: {
            model: "gpt-initial",
            options: [
              { id: "reasoningEffort", value: "low" },
              { id: "serviceTier", value: "default" },
            ],
          },
        },
      },
    });

    await mount(<NewThreadHarness />);

    await act(async () => {
      configureEnvironment({
        providers: [
          serverProvider({
            instanceId: codexInstanceId,
            driver: codexDriver,
            models: [providerModel("gpt-next", [reasoningEffortDescriptor, serviceTierDescriptor])],
          }),
        ],
        settings: {
          providerSessionDefaults: {
            [codexDriver]: {
              model: "gpt-next",
              options: [
                { id: "reasoningEffort", value: "high" },
                { id: "serviceTier", value: "fast" },
              ],
            },
          },
        },
      });
      await Promise.resolve();
    });
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: codexInstanceId,
      model: "gpt-next",
      options: [
        { id: "reasoningEffort", value: "high" },
        { id: "serviceTier", value: "fast" },
      ],
    });
  });

  it("seeds a worktree new draft from the configured provider session default", async () => {
    testState.projects = [project()];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [
            providerModel("gpt-worktree", [reasoningEffortDescriptor, serviceTierDescriptor]),
          ],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: {
            model: "gpt-worktree",
            options: [
              { id: "reasoningEffort", value: "low" },
              { id: "serviceTier", value: "fast" },
            ],
          },
        },
      },
    });

    await mount(<NewThreadHarness options={{ envMode: "worktree" }} />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: codexInstanceId,
      model: "gpt-worktree",
      options: [
        { id: "reasoningEffort", value: "low" },
        { id: "serviceTier", value: "fast" },
      ],
    });
  });

  it("lets the project default route the provider while configured defaults control the selection", async () => {
    const projectSelection = createModelSelection(claudeWorkInstanceId, "claude-project", [
      { id: "effort", value: "high" },
      { id: "fastMode", value: true },
    ]);
    testState.projects = [
      project(projectId, environmentId, { defaultModelSelection: projectSelection }),
    ];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [providerModel("gpt-configured", [reasoningEffortDescriptor])],
        }),
        serverProvider({
          instanceId: claudeWorkInstanceId,
          driver: claudeDriver,
          models: [
            providerModel("claude-project", [claudeEffortDescriptor, fastModeDescriptor]),
            providerModel("claude-configured", [claudeEffortDescriptor, fastModeDescriptor]),
          ],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: { model: "gpt-configured" },
          [claudeDriver]: {
            model: "claude-configured",
            options: [
              { id: "effort", value: "low" },
              { id: "fastMode", value: false },
            ],
          },
        },
      },
    });

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: claudeWorkInstanceId,
      model: "claude-configured",
      options: [
        { id: "effort", value: "low" },
        { id: "fastMode", value: false },
      ],
    });
  });

  it("keeps sticky provider routing but replaces sticky model options with shared defaults", async () => {
    testState.projects = [project()];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: claudeWorkInstanceId,
          driver: claudeDriver,
          models: [
            providerModel("claude-sticky", [claudeEffortDescriptor, fastModeDescriptor]),
            providerModel("claude-current", [claudeEffortDescriptor, fastModeDescriptor]),
          ],
        }),
      ],
      settings: {
        providerInstances: {
          [claudeWorkInstanceId]: { driver: claudeDriver },
        },
        providerSessionDefaults: {
          [claudeDriver]: {
            model: "claude-current",
            options: [
              { id: "effort", value: "low" },
              { id: "fastMode", value: false },
            ],
          },
        },
      },
    });
    useComposerDraftStore.getState().setStickyModelSelection(
      createModelSelection(claudeWorkInstanceId, "claude-sticky", [
        { id: "effort", value: "high" },
        { id: "fastMode", value: true },
      ]),
    );

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: claudeWorkInstanceId,
      model: "claude-current",
      options: [
        { id: "effort", value: "low" },
        { id: "fastMode", value: false },
      ],
    });
  });

  it("falls back within the selected instance without mutating configured defaults", async () => {
    const configuredDefault = {
      model: "gpt-default-instance-only",
      options: [{ id: "reasoningEffort", value: "high" }] as const,
    };
    const settings = {
      providerInstances: {
        [ProviderInstanceId.make("codex_work")]: { driver: codexDriver },
      },
      providerSessionDefaults: {
        [codexDriver]: configuredDefault,
      },
    };
    const codexWorkInstanceId = ProviderInstanceId.make("codex_work");
    testState.projects = [project()];
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [providerModel("gpt-default-instance-only", [reasoningEffortDescriptor])],
        }),
        serverProvider({
          instanceId: codexWorkInstanceId,
          driver: codexDriver,
          models: [
            providerModel("custom-first", [reasoningEffortDescriptor], {
              isCustom: true,
            }),
            providerModel("built-in-second", [reasoningEffortDescriptor]),
          ],
        }),
      ],
      settings,
    });
    useComposerDraftStore.setState({ stickyActiveProvider: codexWorkInstanceId });

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toEqual({
      instanceId: codexWorkInstanceId,
      model: "built-in-second",
      options: [{ id: "reasoningEffort", value: "high" }],
    });
    expect(
      testState.serverConfigs.get(environmentId)?.settings.providerSessionDefaults[codexDriver],
    ).toEqual(configuredDefault);
    expect(console.warn).toHaveBeenCalledWith(
      "Provider session default fallback",
      expect.objectContaining({
        driver: codexDriver,
        instanceId: codexWorkInstanceId,
        configuredModel: "gpt-default-instance-only",
        resolvedModel: "built-in-second",
        reason: "configured-model-unavailable",
      }),
    );
  });

  it("does not borrow provider discovery from another environment", async () => {
    const remoteEnvironmentId = EnvironmentId.make("remote-test");
    testState.projects = [project()];
    configureEnvironment({
      providers: [],
      settings: {
        providerInstances: {
          [claudeWorkInstanceId]: { driver: claudeDriver },
        },
        providerSessionDefaults: {
          [claudeDriver]: { model: "claude-configured" },
        },
      },
    });
    testState.serverConfigs.set(remoteEnvironmentId, {
      providers: [
        serverProvider({
          instanceId: claudeWorkInstanceId,
          driver: claudeDriver,
          models: [providerModel("remote-only-model", [claudeEffortDescriptor])],
        }),
      ],
      settings: {
        ...DEFAULT_SERVER_SETTINGS,
        providerInstances: {
          [claudeWorkInstanceId]: { driver: claudeDriver },
        },
        providerSessionDefaults: {
          [claudeDriver]: { model: "remote-only-model" },
        },
      },
    });
    useComposerDraftStore.setState({ stickyActiveProvider: claudeWorkInstanceId });

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(createdDraftModelSelection()).toMatchObject({
      instanceId: claudeWorkInstanceId,
      model: DEFAULT_MODEL_BY_PROVIDER[claudeDriver],
    });
    expect(console.warn).toHaveBeenCalledWith(
      "Provider session default fallback",
      expect.objectContaining({
        driver: claudeDriver,
        instanceId: claudeWorkInstanceId,
        configuredModel: "claude-configured",
        reason: "models-unavailable",
      }),
    );
  });

  it("does not reseed an already-created reusable draft", async () => {
    const draftId = "draft-with-selection" as never;
    const currentSelection = createModelSelection(codexInstanceId, "gpt-user-choice", [
      { id: "reasoningEffort", value: "low" },
      { id: "serviceTier", value: "default" },
    ]);
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [
            providerModel("gpt-current-default", [
              reasoningEffortDescriptor,
              serviceTierDescriptor,
            ]),
          ],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: {
            model: "gpt-current-default",
            options: [
              { id: "reasoningEffort", value: "high" },
              { id: "serviceTier", value: "fast" },
            ],
          },
        },
      },
    });
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: ThreadId.make("thread-with-selection"),
      });
    useComposerDraftStore.getState().setModelSelection(draftId, currentSelection);
    setRoute({ draftId });

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(
      useComposerDraftStore.getState().getComposerDraft(draftId)?.modelSelectionByProvider[
        codexInstanceId
      ],
    ).toEqual(currentSelection);
  });

  it("does not overwrite an existing server thread selection after defaults change", async () => {
    const serverThreadId = ThreadId.make("thread-existing-server");
    const serverThreadRef = scopeThreadRef(environmentId, serverThreadId);
    const currentSelection = createModelSelection(codexInstanceId, "gpt-server-choice", [
      { id: "reasoningEffort", value: "low" },
    ]);
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [providerModel("gpt-server-choice", [reasoningEffortDescriptor])],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: {
            model: "gpt-server-choice",
            options: [{ id: "reasoningEffort", value: "low" }],
          },
        },
      },
    });
    useComposerDraftStore.getState().setModelSelection(serverThreadRef, currentSelection);
    setRoute({ environmentId, threadId: serverThreadId });

    // A later settings update applies only to future drafts.
    configureEnvironment({
      providers: [
        serverProvider({
          instanceId: codexInstanceId,
          driver: codexDriver,
          models: [providerModel("gpt-current-default", [reasoningEffortDescriptor])],
        }),
      ],
      settings: {
        providerSessionDefaults: {
          [codexDriver]: {
            model: "gpt-current-default",
            options: [{ id: "reasoningEffort", value: "high" }],
          },
        },
      },
    });

    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(
      useComposerDraftStore.getState().getComposerDraft(serverThreadRef)?.modelSelectionByProvider[
        codexInstanceId
      ],
    ).toEqual(currentSelection);
  });

  it("uses project grouping, server configuration, and every explicit option", async () => {
    testState.projects = [project()];
    testState.serverConfigs.set(environmentId, {
      settings: {
        ...DEFAULT_SERVER_SETTINGS,
        defaultThreadEnvMode: "local",
        newWorktreesStartFromOrigin: false,
      },
      providers: [],
    });
    await mount(
      <NewThreadHarness
        options={{
          branch: "feature/coverage",
          worktreePath: "X:\\worktrees\\coverage",
          envMode: "worktree",
          startFromOrigin: true,
        }}
      />,
    );
    await clickNewThread();

    const draft = useComposerDraftStore
      .getState()
      .getDraftSessionByLogicalProjectKey(testState.logicalProjectKey);
    expect(draft).toMatchObject({
      branch: "feature/coverage",
      worktreePath: "X:\\worktrees\\coverage",
      envMode: "worktree",
      startFromOrigin: true,
    });
  });

  it("reuses a stored draft in place and updates only supplied context", async () => {
    const draftId = "draft-stored" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: ThreadId.make("thread-stored"),
        branch: "main",
        envMode: "local",
      });
    setRoute({ draftId });
    await mount(
      <NewThreadHarness
        options={{
          branch: null,
          worktreePath: "X:\\worktrees\\stored",
          envMode: "local",
          startFromOrigin: true,
        }}
      />,
    );
    await clickNewThread();

    expect(testState.router.navigate).not.toHaveBeenCalled();
    expect(useComposerDraftStore.getState().getDraftSession(draftId)).toMatchObject({
      branch: null,
      worktreePath: "X:\\worktrees\\stored",
      envMode: "worktree",
      startFromOrigin: true,
    });
  });

  it.each([
    ["branch only", { branch: "feature/partial" } satisfies NewThreadOptions],
    ["nullable worktree only", { worktreePath: null } satisfies NewThreadOptions],
  ])("reuses a stored draft with %s", async (_label, options) => {
    const draftId = "draft-stored-partial" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: ThreadId.make("thread-stored-partial"),
        branch: "main",
        worktreePath: "X:\\worktrees\\previous",
        envMode: "local",
        startFromOrigin: true,
      });
    setRoute({ draftId });
    await mount(<NewThreadHarness {...(options === undefined ? {} : { options })} />);
    await clickNewThread();

    expect(testState.router.navigate).not.toHaveBeenCalled();
    expect(
      useComposerDraftStore
        .getState()
        .getDraftSessionByLogicalProjectKey(scopedProjectKey(projectRef))?.draftId,
    ).toBe(draftId);
  });

  it("finds a draft stored under the project's physical key and moves it to the current key", async () => {
    testState.projects = [project(projectId, environmentId, { repositoryIdentity: null })];
    // derivePhysicalProjectKey lowercases the workspace path.
    const physicalKey = `${environmentId}:x:\\repos\\${projectId}`;
    const storedDraftId = "draft-physical" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(physicalKey, projectRef, storedDraftId, {
        threadId: ThreadId.make("thread-physical"),
      });
    const lookup = vi.spyOn(useComposerDraftStore.getState(), "getDraftSessionByLogicalProjectKey");
    useComposerDraftStore.setState({ getDraftSessionByLogicalProjectKey: lookup });
    setRoute({ draftId: "draft-other" });
    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(lookup).toHaveBeenCalledWith(testState.logicalProjectKey, [physicalKey], projectRef);
    expect(testState.router.navigate).toHaveBeenCalledWith({
      to: "/draft/$draftId",
      params: { draftId: storedDraftId },
    });
    const mappings =
      useComposerDraftStore.getState().logicalProjectDraftThreadKeyByLogicalProjectKey;
    expect(mappings[testState.logicalProjectKey]).toBe(storedDraftId);
    expect(mappings[physicalKey]).toBeUndefined();
  });

  it("navigates back to a reusable stored draft from another route", async () => {
    const storedDraftId = "draft-stored" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, storedDraftId, {
        threadId: ThreadId.make("thread-stored"),
      });
    setRoute({ draftId: "draft-other" });
    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(testState.router.navigate).toHaveBeenCalledWith({
      to: "/draft/$draftId",
      params: { draftId: storedDraftId },
    });
  });

  it("reuses the active draft when its logical-project pointer is temporarily absent", async () => {
    const draftId = "draft-active" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: ThreadId.make("thread-active"),
        envMode: "local",
      });
    useComposerDraftStore.setState({
      logicalProjectDraftThreadKeyByLogicalProjectKey: {},
    });
    setRoute({ draftId });
    await mount(
      <NewThreadHarness
        options={{
          branch: "feature/reuse",
          worktreePath: "X:\\worktrees\\active",
          envMode: "worktree",
          startFromOrigin: false,
        }}
      />,
    );
    await clickNewThread();

    expect(testState.router.navigate).not.toHaveBeenCalled();
    expect(useComposerDraftStore.getState().getDraftSession(draftId)).toMatchObject({
      branch: "feature/reuse",
      worktreePath: "X:\\worktrees\\active",
      envMode: "worktree",
      startFromOrigin: false,
    });
    expect(
      useComposerDraftStore
        .getState()
        .getDraftSessionByLogicalProjectKey(scopedProjectKey(projectRef))?.draftId,
    ).toBe(draftId);
  });

  it.each([
    ["without overrides", undefined],
    ["with only a branch", { branch: "feature/partial" } satisfies NewThreadOptions],
    ["with only a nullable worktree", { worktreePath: null } satisfies NewThreadOptions],
  ])("reuses a detached active draft %s", async (_label, options) => {
    const draftId = "draft-active-partial" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: ThreadId.make("thread-active-partial"),
        branch: "main",
        worktreePath: "X:\\worktrees\\previous",
        envMode: "local",
        startFromOrigin: true,
      });
    useComposerDraftStore.setState({
      logicalProjectDraftThreadKeyByLogicalProjectKey: {},
    });
    setRoute({ draftId });
    await mount(<NewThreadHarness {...(options === undefined ? {} : { options })} />);
    await clickNewThread();

    expect(testState.router.navigate).not.toHaveBeenCalled();
    expect(
      useComposerDraftStore
        .getState()
        .getDraftSessionByLogicalProjectKey(scopedProjectKey(projectRef))?.draftId,
    ).toBe(draftId);
  });

  it("marks a server-backed stored draft promoted before creating a replacement", async () => {
    const storedDraftId = "draft-promoted" as never;
    const storedThreadId = ThreadId.make("thread-promoted");
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, storedDraftId, {
        threadId: storedThreadId,
      });
    testState.shellExists = true;
    await mount(<NewThreadHarness />);
    await clickNewThread();

    const replacement = useComposerDraftStore
      .getState()
      .getDraftSessionByLogicalProjectKey(scopedProjectKey(projectRef));
    expect(replacement?.draftId).not.toBe(storedDraftId);
    expect(useComposerDraftStore.getState().getDraftSession(storedDraftId)?.promotedTo).toEqual(
      scopeThreadRef(environmentId, storedThreadId),
    );
    expect(testState.router.navigate).toHaveBeenCalledWith(
      expect.objectContaining({ params: { draftId: replacement!.draftId } }),
    );
  });

  it("creates a fresh draft when the matching active draft is reached through a server route", async () => {
    const activeDraftId = "draft-from-server" as never;
    const activeThreadId = ThreadId.make("thread-from-server");
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, activeDraftId, {
        threadId: activeThreadId,
      });
    useComposerDraftStore.setState({
      logicalProjectDraftThreadKeyByLogicalProjectKey: {},
    });
    setRoute({ environmentId, threadId: activeThreadId });
    await mount(<NewThreadHarness />);
    await clickNewThread();

    const replacement = useComposerDraftStore
      .getState()
      .getDraftSessionByLogicalProjectKey(scopedProjectKey(projectRef));
    expect(replacement?.draftId).not.toBe(activeDraftId);
    expect(testState.router.navigate).toHaveBeenCalledWith(
      expect.objectContaining({ params: { draftId: replacement!.draftId } }),
    );
  });

  it("treats an empty router match stack as an idle route", async () => {
    testState.router.state.matches = [];
    await mount(<NewThreadHarness />);
    await clickNewThread();

    expect(testState.router.navigate).toHaveBeenCalledWith(
      expect.objectContaining({ to: "/draft/$draftId" }),
    );
  });
});

describe("useHandleNewThread", () => {
  it("orders the default project and reports an idle route", async () => {
    const secondProjectId = ProjectId.make("project-second");
    testState.projects = [project(), project(secondProjectId)];
    testState.projectOrder = [`${environmentId}:${secondProjectId}`];
    await mount(<HandleSummary />);

    const summary = document.querySelector<HTMLOutputElement>('[data-testid="summary"]')!;
    expect(summary.dataset.defaultProject).toBe(secondProjectId);
    expect(summary.dataset.routeThread).toBe("");
    expect(summary.dataset.activeThread).toBe("no");
    expect(summary.dataset.activeDraft).toBe("");
    expect(summary.dataset.handler).toBe("function");
  });

  it("reports server route, server entity, and matching promoted draft state", async () => {
    const routeThreadId = ThreadId.make("route-thread");
    const routeRef = scopeThreadRef(environmentId, routeThreadId);
    const draftId = "draft-server-route" as never;
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: routeThreadId,
      });
    testState.activeThread = { id: routeThreadId };
    setRoute({ environmentId, threadId: routeThreadId });
    await mount(<HandleSummary />);

    const summary = document.querySelector<HTMLOutputElement>('[data-testid="summary"]')!;
    expect(summary.dataset.routeThread).toBe(routeRef.threadId);
    expect(summary.dataset.activeThread).toBe("yes");
    expect(summary.dataset.activeDraft).toBe(routeThreadId);
  });

  it("reports draft route state without a server thread", async () => {
    const draftId = "draft-route" as never;
    const draftThreadId = ThreadId.make("draft-route-thread");
    useComposerDraftStore
      .getState()
      .setLogicalProjectDraftThreadId(scopedProjectKey(projectRef), projectRef, draftId, {
        threadId: draftThreadId,
      });
    setRoute({ draftId });
    await mount(<HandleSummary />);

    const summary = document.querySelector<HTMLOutputElement>('[data-testid="summary"]')!;
    expect(summary.dataset.routeThread).toBe("");
    expect(summary.dataset.activeThread).toBe("no");
    expect(summary.dataset.activeDraft).toBe(draftThreadId);
  });
});
