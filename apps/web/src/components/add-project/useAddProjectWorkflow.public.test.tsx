// @vitest-environment happy-dom

import {
  DEFAULT_SERVER_SETTINGS,
  EnvironmentId,
  GitCommandError,
  ProviderDriverKind,
  ProviderInstanceId,
  ThreadId,
  type ModelSelection,
  type ServerProvider,
} from "@bibcode/contracts";
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import type { ConnectionTarget } from "@bibcode/client-runtime/connection";
import * as Cause from "effect/Cause";
import { AsyncResult } from "effect/unstable/reactivity";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { EnvironmentPresentationPolicy } from "~/connection/environmentPresentationPolicy";

type AddProjectPresentation = Pick<
  EnvironmentPresentationPolicy,
  "surface" | "platform" | "presentsTarget"
>;

const harness = vi.hoisted(() => ({
  environments: [] as unknown[],
  desktopLocalBootstraps: [] as Array<{ readonly id: string; readonly httpBaseUrl: string }>,
  presentation: {
    surface: "browser" as const,
    platform: "unknown" as const,
    presentsTarget: (_target: ConnectionTarget) => true,
  } as AddProjectPresentation,
  projects: [] as unknown[],
  createProject: vi.fn(),
  cloneRepository: vi.fn(),
  cancelClone: vi.fn(),
  createThread: vi.fn(),
  navigate: vi.fn(),
  readEnvironmentThreadRefs: vi.fn(),
  readThreadShell: vi.fn(),
  replaceMainWithTerminal: vi.fn(),
  onOpenChange: vi.fn(),
  activeEnvironmentId: null as EnvironmentId | null,
}));

vi.mock("~/connection/useDesktopLocalBootstraps", () => ({
  useDesktopLocalBootstraps: () => harness.desktopLocalBootstraps,
}));

vi.mock("~/connection/currentEnvironmentPresentation", () => ({
  readCurrentEnvironmentPresentationPolicy: () => harness.presentation,
}));

vi.mock("@tanstack/react-router", () => ({
  useNavigate: () => harness.navigate,
}));

vi.mock("~/centerPanelStore", () => ({
  useCenterPanelStore: {
    getState: () => ({ replaceMainWithTerminal: harness.replaceMainWithTerminal }),
  },
}));

vi.mock("~/state/environments", () => ({
  useEnvironments: () => ({
    isReady: true,
    networkStatus: "online",
    environments: harness.environments,
  }),
  usePrimaryEnvironment: () => harness.environments[0] ?? null,
}));

vi.mock("~/state/entities", () => ({
  useProjects: () => harness.projects,
  useActiveEnvironmentId: () => harness.activeEnvironmentId,
  readEnvironmentThreadRefs: (environmentId: unknown) =>
    harness.readEnvironmentThreadRefs(environmentId),
  readThreadShell: (ref: unknown) => harness.readThreadShell(ref),
}));

vi.mock("~/state/projects", () => ({
  projectEnvironment: {
    create: { key: "project.create" },
  },
}));

vi.mock("~/state/vcs", () => ({
  vcsEnvironment: {
    clone: { key: "vcs.clone" },
    cancelClone: { key: "vcs.cancelClone" },
  },
}));

vi.mock("~/state/threads", () => ({
  threadEnvironment: {
    create: { key: "thread.create" },
  },
}));

vi.mock("~/state/use-atom-command", () => ({
  useAtomCommand: (command: { readonly key?: string }) => {
    if (command.key === "project.create") {
      return (input: unknown) => harness.createProject(input);
    }
    if (command.key === "vcs.clone") {
      return (input: unknown, runOptions?: unknown) => harness.cloneRepository(input, runOptions);
    }
    if (command.key === "vcs.cancelClone") {
      return (input: unknown) => harness.cancelClone(input);
    }
    if (command.key === "thread.create") {
      return (input: unknown) => harness.createThread(input);
    }
    throw new Error(`Unexpected atom command: ${String(command.key)}`);
  },
}));

import { useAddProjectWorkflow, type AddProjectWorkflow } from "./useAddProjectWorkflow";

const environmentId = EnvironmentId.make("public-workflow");
const wslEnvironmentId = EnvironmentId.make("wsl-workflow");
const sshEnvironmentId = EnvironmentId.make("ssh-workflow");
const relayEnvironmentId = EnvironmentId.make("relay-workflow");
const bearerEnvironmentId = EnvironmentId.make("bearer-workflow");
const defaultThreadId = ThreadId.make("canonical-main");
const codexInstanceId = ProviderInstanceId.make("codex");
const claudeInstanceId = ProviderInstanceId.make("claudeAgent");
const expectedSelection: ModelSelection = {
  instanceId: codexInstanceId,
  model: "gpt-5.4",
  options: [
    { id: "reasoningEffort", value: "high" },
    { id: "serviceTier", value: "fast" },
  ],
};

const provider: ServerProvider = {
  instanceId: codexInstanceId,
  driver: ProviderDriverKind.make("codex"),
  enabled: true,
  installed: true,
  version: "1.0.0",
  status: "ready",
  auth: { status: "authenticated" },
  checkedAt: "2026-07-20T00:00:00.000Z",
  models: [
    {
      slug: "gpt-5.4",
      name: "GPT-5.4",
      isCustom: false,
      capabilities: {
        optionDescriptors: [
          {
            id: "reasoningEffort",
            label: "Reasoning",
            type: "select",
            options: [
              { id: "medium", label: "Medium", isDefault: true },
              { id: "high", label: "High" },
            ],
            currentValue: "medium",
          },
          {
            id: "serviceTier",
            label: "Service tier",
            type: "select",
            options: [
              { id: "default", label: "Default", isDefault: true },
              { id: "fast", label: "Fast" },
            ],
            currentValue: "default",
          },
        ],
      },
    },
  ],
  slashCommands: [],
  skills: [],
  agents: [],
};
const claudeProvider: ServerProvider = {
  ...provider,
  instanceId: claudeInstanceId,
  driver: ProviderDriverKind.make("claudeAgent"),
  models: [
    {
      slug: "claude-opus-4-1",
      name: "Claude Opus 4.1",
      isCustom: false,
      capabilities: {},
    },
  ],
};

function environment(
  environmentId: EnvironmentId,
  label: string,
  target: Record<string, unknown>,
  os: "darwin" | "linux" = "linux",
) {
  const displayUrl = `http://${environmentId}.test:4317`;
  return {
    environmentId,
    label,
    displayUrl,
    relayManaged: false,
    entry: {
      target: {
        environmentId,
        label,
        ...target,
      },
    },
    connection: { phase: "connected", error: null, traceId: null },
    serverConfig: {
      environment: {
        environmentId,
        label,
        platform: { os, arch: "arm64" },
        serverVersion: "0.2.3",
        capabilities: { repositoryIdentity: true },
      },
      providers: [provider, claudeProvider],
      settings: {
        ...DEFAULT_SERVER_SETTINGS,
        addProjectBaseDirectory: "/code/",
        defaultAgent: { kind: "chat", instanceId: claudeInstanceId },
        providerSessionDefaults: {
          ...DEFAULT_SERVER_SETTINGS.providerSessionDefaults,
          codex: { model: expectedSelection.model, options: expectedSelection.options },
          claudeAgent: { model: "claude-opus-4-1", options: [] },
        },
      },
    },
  };
}

function presentation(
  surface: "browser" | "desktop",
  platform: "macos" | "windows",
): AddProjectPresentation {
  return {
    surface,
    platform,
    presentsTarget: (target) => {
      if (surface === "browser" || target._tag === "PrimaryConnectionTarget") return true;
      return (
        platform === "windows" &&
        "connectionId" in target &&
        target.connectionId.startsWith("local:")
      );
    },
  };
}

let currentWorkflow: AddProjectWorkflow;
let root: Root | null = null;
let container: HTMLDivElement | null = null;

function captureWorkflow(workflow: AddProjectWorkflow) {
  currentWorkflow = workflow;
}

function WorkflowProbe({
  onWorkflow,
}: {
  readonly onWorkflow: (workflow: AddProjectWorkflow) => void;
}) {
  const workflow = useAddProjectWorkflow({
    open: true,
    onOpenChange: harness.onOpenChange,
  });
  onWorkflow(workflow);
  return null;
}

async function mountWorkflow(): Promise<AddProjectWorkflow> {
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
  await act(async () => root?.render(<WorkflowProbe onWorkflow={captureWorkflow} />));
  return currentWorkflow;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  harness.environments = [
    environment(
      environmentId,
      "Local",
      {
        _tag: "PrimaryConnectionTarget",
        httpBaseUrl: "http://public-workflow.test:4317",
        wsBaseUrl: "ws://public-workflow.test:4317",
      },
      "darwin",
    ),
  ];
  harness.desktopLocalBootstraps = [];
  harness.presentation = presentation("browser", "macos");
  harness.projects = [];
  harness.createProject.mockReset().mockImplementation(async (command) => {
    const input = command as { readonly input: { readonly projectId: string } };
    return AsyncResult.success({ projectId: input.input.projectId, threadId: defaultThreadId });
  });
  harness.cloneRepository
    .mockReset()
    .mockResolvedValue(AsyncResult.success({ path: "/code/cloned" }));
  harness.cancelClone.mockReset().mockResolvedValue(AsyncResult.success({ cancelled: true }));
  harness.createThread.mockReset().mockResolvedValue(AsyncResult.success({ sequence: 1 }));
  harness.navigate.mockReset().mockResolvedValue(undefined);
  harness.readEnvironmentThreadRefs.mockReset().mockReturnValue([]);
  harness.readThreadShell.mockReset().mockReturnValue(null);
  harness.replaceMainWithTerminal.mockReset();
  harness.onOpenChange.mockReset();
  harness.activeEnvironmentId = null;
});

afterEach(async () => {
  if (root !== null) {
    await act(async () => root?.unmount());
  }
  container?.remove();
  root = null;
  container = null;
  document.body.replaceChildren();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("useAddProjectWorkflow public adapter", () => {
  it("keeps saved remote hosts on desktop and defaults to the rail selection", async () => {
    harness.environments = [
      ...harness.environments,
      environment(sshEnvironmentId, "Build server", {
        _tag: "SshConnectionTarget",
        connectionId: "ssh:build-server",
      }),
    ];
    harness.presentation = presentation("desktop", "macos");
    harness.activeEnvironmentId = sshEnvironmentId;

    await mountWorkflow();

    expect(currentWorkflow.hosts.map((host) => host.label)).toEqual([
      "This device",
      "Build server",
    ]);
    expect(currentWorkflow.selectedHost.environmentId).toBe(sshEnvironmentId);
  });

  it("presents this device and saved remote hosts on macOS desktop", async () => {
    harness.environments = [
      ...harness.environments,
      environment(wslEnvironmentId, "Ubuntu (WSL)", {
        _tag: "BearerConnectionTarget",
        connectionId: "local:wsl:Ubuntu",
      }),
      environment(sshEnvironmentId, "Build server", {
        _tag: "SshConnectionTarget",
        connectionId: "ssh:build-server",
      }),
      environment(relayEnvironmentId, "Relay server", { _tag: "RelayConnectionTarget" }),
      environment(bearerEnvironmentId, "Remote server", {
        _tag: "BearerConnectionTarget",
        connectionId: "remote:server",
      }),
    ];
    harness.presentation = presentation("desktop", "macos");

    await mountWorkflow();

    expect(currentWorkflow.hosts.map((host) => host.label)).toEqual([
      "This device",
      "Build server",
      "Relay server",
      "Remote server",
    ]);
    // Without the rail (Repositories view) the picker is the only way to choose a server.
    expect(currentWorkflow).toHaveProperty("locationLabel", "Host");
  });

  it("presents primary, WSL, and saved remote locations on Windows desktop", async () => {
    const wsl = environment(wslEnvironmentId, "Ubuntu (WSL)", {
      _tag: "BearerConnectionTarget",
      connectionId: "local:wsl:Ubuntu",
    });
    harness.environments = [
      ...harness.environments,
      wsl,
      environment(sshEnvironmentId, "Build server", {
        _tag: "SshConnectionTarget",
        connectionId: "ssh:build-server",
      }),
      environment(relayEnvironmentId, "Relay server", { _tag: "RelayConnectionTarget" }),
      environment(bearerEnvironmentId, "Remote server", {
        _tag: "BearerConnectionTarget",
        connectionId: "remote:server",
      }),
    ];
    harness.desktopLocalBootstraps = [{ id: "wsl:Ubuntu", httpBaseUrl: wsl.displayUrl }];
    harness.presentation = presentation("desktop", "windows");

    await mountWorkflow();

    expect(currentWorkflow.hosts.map((host) => host.label)).toEqual([
      "This device",
      "Ubuntu (WSL)",
      "Build server",
      "Relay server",
      "Remote server",
    ]);
    expect(currentWorkflow).toHaveProperty("locationLabel", "Location");
  });

  it("excludes an unavailable WSL location without a current bootstrap on Windows desktop", async () => {
    harness.environments = [
      ...harness.environments,
      environment(wslEnvironmentId, "Ubuntu (WSL)", {
        _tag: "UnavailableConnectionTarget",
        configuredDistro: "Ubuntu",
        connectionId: "local:wsl:Ubuntu",
        detail: "WSL is unavailable",
      }),
    ];
    harness.presentation = presentation("desktop", "windows");

    await mountWorkflow();

    expect(currentWorkflow.hosts.map((host) => host.label)).toEqual(["This device"]);
    expect(currentWorkflow).toHaveProperty("locationLabel", null);
  });

  it("preserves unavailable WSL and remote hosts with the Host selector in browser mode", async () => {
    harness.environments = [
      ...harness.environments,
      environment(wslEnvironmentId, "Ubuntu (WSL)", {
        _tag: "UnavailableConnectionTarget",
        configuredDistro: "Ubuntu",
        connectionId: "local:wsl:Ubuntu",
        detail: "WSL is unavailable",
      }),
      environment(sshEnvironmentId, "Build server", {
        _tag: "SshConnectionTarget",
        connectionId: "ssh:build-server",
      }),
      environment(relayEnvironmentId, "Relay server", { _tag: "RelayConnectionTarget" }),
      environment(bearerEnvironmentId, "Remote server", {
        _tag: "BearerConnectionTarget",
        connectionId: "remote:server",
      }),
    ];

    await mountWorkflow();

    expect(currentWorkflow.hosts.map((host) => host.label)).toEqual([
      "This device",
      "Ubuntu (WSL)",
      "Build server",
      "Relay server",
      "Remote server",
    ]);
    expect(currentWorkflow).toHaveProperty("locationLabel", "Host");
  });

  it.each(["add", "clone", "create"] as const)(
    "opens canonical Main with the selected chat default for the %s flow",
    async (flow) => {
      const workflow = await mountWorkflow();

      if (flow === "add") {
        act(() => currentWorkflow.setHostPath("/code/added"));
        await act(async () => currentWorkflow.submitHostPath());
      } else if (flow === "clone") {
        act(() => currentWorkflow.openClone());
        act(() => currentWorkflow.setCloneUrl("https://example.test/repository.git"));
        act(() => currentWorkflow.setCloneParent("/code"));
        await act(async () => currentWorkflow.submitClone());
      } else {
        act(() => currentWorkflow.openCreate());
        act(() => currentWorkflow.setCreateName("created"));
        act(() => currentWorkflow.setCreateParent("/code"));
        await act(async () => currentWorkflow.submitCreate());
      }

      expect(workflow.selectedHost.environmentId).toBe(environmentId);
      expect(harness.createProject).toHaveBeenCalledTimes(1);
      expect(harness.createProject.mock.calls[0]?.[0]).toMatchObject({
        environmentId,
        input: {
          defaultModelSelection: expect.objectContaining({ instanceId: claudeInstanceId }),
        },
      });
      expect(harness.navigate).toHaveBeenCalledWith({
        to: "/$environmentId/$threadId",
        params: { environmentId, threadId: defaultThreadId },
      });
      expect(harness.replaceMainWithTerminal).not.toHaveBeenCalled();
      expect(harness.onOpenChange).toHaveBeenCalledWith(false);
    },
  );

  it("interrupts the clone command when the clone is cancelled", async () => {
    let signal: AbortSignal | undefined;
    harness.cloneRepository.mockImplementation(
      (_input: unknown, runOptions?: { readonly signal?: AbortSignal }) =>
        new Promise((resolve) => {
          signal = runOptions?.signal;
          signal?.addEventListener("abort", () => resolve(AsyncResult.failure(Cause.interrupt())), {
            once: true,
          });
        }),
    );
    await mountWorkflow();
    act(() => currentWorkflow.openClone());
    act(() => currentWorkflow.setCloneUrl("https://example.test/repository.git"));
    act(() => currentWorkflow.setCloneParent("/code"));
    let submission!: Promise<void>;
    act(() => {
      submission = currentWorkflow.submitClone();
    });

    expect(harness.cloneRepository).toHaveBeenCalledWith(
      {
        environmentId,
        input: {
          url: "https://example.test/repository.git",
          parentDir: "/code",
          onProgress: expect.any(Function),
        },
      },
      { signal: expect.any(AbortSignal) },
    );
    expect(currentWorkflow.cloneProgress).toBe("cloning");

    await act(async () => {
      currentWorkflow.cancelClone();
      await submission;
    });

    expect(signal?.aborted).toBe(true);
    expect(currentWorkflow.notice).toBe("Clone cancelled.");
    expect(currentWorkflow.error).toBeNull();
    expect(currentWorkflow.busy).toBe(false);
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.onOpenChange).not.toHaveBeenCalledWith(false);
    expect(harness.cancelClone).not.toHaveBeenCalled();
  });

  it("shows the server's clone failure detail without the RPC wrapper", async () => {
    // Test-owned text: the form shows whatever detail the server sends, unchanged.
    const detail = "Synthetic server detail: the destination needs attention.";
    harness.cloneRepository.mockResolvedValueOnce(
      AsyncResult.failure(
        Cause.fail(
          new GitCommandError({
            operation: "GitVcsDriver.cloneRepository",
            command: "git clone",
            cwd: "/code",
            detail,
          }),
        ),
      ),
    );
    await mountWorkflow();
    act(() => currentWorkflow.openClone());
    act(() => currentWorkflow.setCloneUrl("https://example.test/repository.git"));
    act(() => currentWorkflow.setCloneParent("/code"));

    await act(async () => currentWorkflow.submitClone());

    expect(currentWorkflow.error).toBe(`Clone failed: ${detail}`);
    expect(currentWorkflow.busy).toBe(false);
    expect(currentWorkflow.step).toBe("clone");
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("replaces canonical Main with the selected terminal default", async () => {
    const environment = harness.environments[0] as {
      serverConfig: { settings: typeof DEFAULT_SERVER_SETTINGS };
    };
    environment.serverConfig.settings = {
      ...environment.serverConfig.settings,
      defaultAgent: { kind: "terminal", instanceId: codexInstanceId },
    };
    await mountWorkflow();

    act(() => currentWorkflow.setHostPath("/code/terminal"));
    await act(async () => currentWorkflow.submitHostPath());

    expect(harness.createProject.mock.calls[0]?.[0]).toMatchObject({
      environmentId,
      input: {
        defaultModelSelection: expect.objectContaining({ instanceId: codexInstanceId }),
      },
    });
    expect(harness.replaceMainWithTerminal).toHaveBeenCalledWith(
      scopeThreadRef(environmentId, defaultThreadId),
      [],
      expect.objectContaining({
        label: "Codex Terminal",
        command: expect.objectContaining({ executable: "codex" }),
      }),
    );
    expect(harness.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId, threadId: defaultThreadId },
    });
  });

  it("falls back from an unavailable saved provider to the first ready chat", async () => {
    const environment = harness.environments[0] as {
      serverConfig: {
        providers: ServerProvider[];
        settings: typeof DEFAULT_SERVER_SETTINGS;
      };
    };
    environment.serverConfig.providers = [claudeProvider, provider];
    environment.serverConfig.settings = {
      ...environment.serverConfig.settings,
      defaultAgent: {
        kind: "terminal",
        instanceId: ProviderInstanceId.make("missing-provider"),
      },
    };
    await mountWorkflow();

    act(() => currentWorkflow.setHostPath("/code/fallback"));
    await act(async () => currentWorkflow.submitHostPath());

    expect(harness.createProject.mock.calls[0]?.[0]).toMatchObject({
      input: {
        defaultModelSelection: expect.objectContaining({ instanceId: claudeInstanceId }),
      },
    });
    expect(harness.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId, threadId: defaultThreadId },
    });
  });

  it("uses project.create to find an existing registered project's canonical Main before the thread cache arrives", async () => {
    const existingProjectId = "existing-project";
    const canonicalProjectId = "canonical-project";
    const canonicalThreadId = ThreadId.make("canonical-existing-main");
    harness.projects = [
      {
        id: existingProjectId,
        environmentId,
        workspaceRoot: "/code/existing",
      },
    ];
    harness.createProject.mockResolvedValueOnce(
      AsyncResult.success({ projectId: canonicalProjectId, threadId: canonicalThreadId }),
    );
    await mountWorkflow();

    act(() => currentWorkflow.setHostPath("/code/existing"));
    await act(async () => currentWorkflow.submitHostPath());

    expect(harness.createProject).toHaveBeenCalledWith({
      environmentId,
      input: expect.objectContaining({
        workspaceRoot: "/code/existing",
        createWorkspaceRootIfMissing: false,
        initializeGit: false,
      }),
    });
    expect(harness.createProject.mock.calls[0]?.[0].input.projectId).not.toBe(existingProjectId);
    expect(harness.createThread).not.toHaveBeenCalled();
    expect(harness.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId, threadId: canonicalThreadId },
    });
  });

  it("creates the legacy safety-net default thread when canonicalization returns no Main", async () => {
    const existingProjectId = "legacy-project";
    harness.projects = [
      {
        id: existingProjectId,
        environmentId,
        workspaceRoot: "/code/legacy",
      },
    ];
    harness.createProject.mockResolvedValueOnce(
      AsyncResult.success({ projectId: existingProjectId }),
    );
    await mountWorkflow();

    act(() => currentWorkflow.setHostPath("/code/legacy"));
    await act(async () => currentWorkflow.submitHostPath());

    expect(harness.createProject).toHaveBeenCalledTimes(1);
    expect(harness.createThread).toHaveBeenCalledWith({
      environmentId,
      input: expect.objectContaining({
        projectId: existingProjectId,
        modelSelection: expect.objectContaining({ instanceId: claudeInstanceId }),
      }),
    });
    expect(harness.createThread.mock.calls[0]?.[0].input).not.toHaveProperty("kind");
    expect(harness.createThread.mock.calls[0]?.[0].input).not.toHaveProperty("worktreePath");
    const createdThreadId = harness.createThread.mock.calls[0]?.[0].input.threadId;
    expect(harness.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId, threadId: createdThreadId },
    });
  });

  it("keeps the canonical Main chat fallback when no provider is ready", async () => {
    const environment = harness.environments[0] as {
      serverConfig: {
        providers: ServerProvider[];
        settings: typeof DEFAULT_SERVER_SETTINGS;
      };
    };
    environment.serverConfig.providers = [
      { ...provider, status: "error" },
      { ...claudeProvider, status: "error" },
    ];
    environment.serverConfig.settings = {
      ...environment.serverConfig.settings,
      defaultAgent: { kind: "terminal", instanceId: codexInstanceId },
    };
    await mountWorkflow();

    act(() => currentWorkflow.setHostPath("/code/no-ready-provider"));
    await act(async () => currentWorkflow.submitHostPath());

    expect(harness.createProject.mock.calls[0]?.[0]).toMatchObject({
      input: {
        defaultModelSelection: expect.objectContaining({ instanceId: codexInstanceId }),
      },
    });
    expect(harness.replaceMainWithTerminal).not.toHaveBeenCalled();
    expect(harness.navigate).toHaveBeenCalledWith({
      to: "/$environmentId/$threadId",
      params: { environmentId, threadId: defaultThreadId },
    });
  });
});
