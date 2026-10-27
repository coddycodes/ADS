// @vitest-environment happy-dom

import { VcsCloneStoppedError } from "@coddy/client-runtime/state/vcs";
import {
  EnvironmentId,
  GitCloneOperationError,
  GitCommandError,
  ThreadId,
  type ProjectId,
} from "@coddy/contracts";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

import type { AddProjectHostOption } from "./AddProjectDialog.logic";
import {
  createAddProjectOperations,
  type AddProjectCommandResult,
  type AddProjectOperationsDependencies,
  type AddProjectOutcome,
} from "./addProjectOperations";
import type { PickAddProjectFolderResult } from "./pickAddProjectFolder";
import { useAddProjectWorkflowState, type AddProjectWorkflow } from "./useAddProjectWorkflow";

type WorkflowOperations = Pick<
  ReturnType<typeof createAddProjectOperations>,
  "addFolder" | "clone" | "create" | "cancelClone"
>;

const ENV_PRIMARY = EnvironmentId.make("primary");
const ENV_REMOTE = EnvironmentId.make("remote");
const ENV_WSL = EnvironmentId.make("wsl");
const DEFAULT_THREAD_ID = ThreadId.make("default-thread");
const OPENED: AddProjectOutcome = { _tag: "Opened" };

const primaryHost: AddProjectHostOption = {
  environmentId: ENV_PRIMARY,
  label: "Local",
  platform: "MacIntel",
  baseDirectory: "~/",
  isPrimary: true,
  desktopInstanceId: null,
  nativePickerAvailable: true,
};
const remoteHost: AddProjectHostOption = {
  environmentId: ENV_REMOTE,
  label: "Remote",
  platform: "Linux",
  baseDirectory: "/srv/code/",
  isPrimary: false,
  desktopInstanceId: null,
  nativePickerAvailable: true,
};
const wslHost: AddProjectHostOption = {
  environmentId: ENV_WSL,
  label: "Ubuntu",
  platform: "Linux",
  baseDirectory: "~/",
  isPrimary: false,
  desktopInstanceId: "wsl:Ubuntu",
  nativePickerAvailable: true,
};

const testState = {
  hosts: [primaryHost, remoteHost, wslHost] as ReadonlyArray<AddProjectHostOption>,
  locationLabel: "Host" as const,
  pickResult: { _tag: "Cancelled" } as PickAddProjectFolderResult,
  pickFolder: vi.fn(async () => testState.pickResult),
  operations: {
    addFolder: vi.fn(async () => true),
    clone: vi.fn(async (): Promise<AddProjectOutcome> => OPENED),
    create: vi.fn(async () => true),
    cancelClone: vi.fn(async () => ({ _tag: "Success" as const, value: { cancelled: true } })),
  },
  operationOverride: null as WorkflowOperations | null,
  onOpenChange: vi.fn(),
  initialEnvironmentId: null as EnvironmentId | null,
};

let currentWorkflow: AddProjectWorkflow;
let workflowRoot: Root | null = null;
let workflowContainer: HTMLDivElement | null = null;

function captureWorkflow(workflow: AddProjectWorkflow) {
  currentWorkflow = workflow;
}

function WorkflowProbe({
  open,
  state,
  onWorkflow,
}: {
  readonly open: boolean;
  readonly state: typeof testState;
  readonly onWorkflow: (workflow: AddProjectWorkflow) => void;
}) {
  const workflow = useAddProjectWorkflowState({
    open,
    onOpenChange: state.onOpenChange,
    hosts: state.hosts,
    locationLabel: state.locationLabel,
    primaryEnvironmentId: ENV_PRIMARY,
    initialEnvironmentId: state.initialEnvironmentId,
    operations: state.operationOverride ?? state.operations,
    pickFolder: state.pickFolder,
  });
  onWorkflow(workflow);
  return null;
}

async function mountWorkflow({ open }: { readonly open: boolean }) {
  workflowContainer = document.createElement("div");
  document.body.append(workflowContainer);
  workflowRoot = createRoot(workflowContainer);
  const rerender = async (
    nextOpen: boolean,
    hosts: ReadonlyArray<AddProjectHostOption> = testState.hosts,
  ) => {
    testState.hosts = hosts;
    await act(async () =>
      workflowRoot?.render(
        <WorkflowProbe open={nextOpen} state={testState} onWorkflow={captureWorkflow} />,
      ),
    );
  };
  await rerender(open);
  return {
    get current(): AddProjectWorkflow {
      return currentWorkflow;
    },
    rerender,
  };
}

async function flushPromises(): Promise<void> {
  await act(async () => {
    await Promise.resolve();
  });
}

function deferredResult<T>(): {
  readonly promise: Promise<T>;
  readonly resolve: (value: T) => void;
} {
  let resolveResult!: (value: T) => void;
  const promise = new Promise<T>((resolve) => {
    resolveResult = resolve;
  });
  return { promise, resolve: resolveResult };
}

function makeIntegratedOperations() {
  const createProject = vi.fn<AddProjectOperationsDependencies["createProject"]>(async (input) => ({
    _tag: "Success",
    value: { projectId: input.projectId, defaultThreadId: DEFAULT_THREAD_ID },
  }));
  const cloneRepository = vi.fn<AddProjectOperationsDependencies["cloneRepository"]>(async () => ({
    _tag: "Success",
    value: { path: "/code/cloned" },
  }));
  const openProject = vi.fn<AddProjectOperationsDependencies["openProject"]>(async () => ({
    _tag: "Success",
    value: undefined,
  }));
  const reportFailure = vi.fn<AddProjectOperationsDependencies["reportFailure"]>();
  const cancelClone = vi.fn<AddProjectOperationsDependencies["cancelClone"]>(async () => ({
    _tag: "Success",
    value: { cancelled: true },
  }));
  const operations = createAddProjectOperations({
    getProjects: () => [],
    createProject,
    cloneRepository,
    cancelClone,
    openProject,
    reportFailure,
  });
  return {
    operations,
    createProject,
    cloneRepository,
    cancelClone,
    openProject,
    reportFailure,
  };
}

/** Clone requests stay pending until their signal aborts, like an interrupted RPC. */
function interruptibleClone(harness: ReturnType<typeof makeIntegratedOperations>) {
  const signals: Array<AbortSignal | undefined> = [];
  harness.cloneRepository.mockImplementation(
    (input) =>
      new Promise((resolve) => {
        signals.push(input.signal);
        input.signal?.addEventListener("abort", () => resolve({ _tag: "Failure", error: null }), {
          once: true,
        });
      }),
  );
  return signals;
}

beforeEach(() => {
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  testState.hosts = [primaryHost, remoteHost, wslHost];
  testState.pickResult = { _tag: "Cancelled" };
  testState.pickFolder.mockReset().mockImplementation(async () => testState.pickResult);
  testState.operations.addFolder.mockReset().mockResolvedValue(true);
  testState.operations.clone.mockReset().mockResolvedValue(OPENED);
  testState.operations.create.mockReset().mockResolvedValue(true);
  testState.operations.cancelClone
    .mockReset()
    .mockResolvedValue({ _tag: "Success", value: { cancelled: true } });
  testState.operationOverride = null;
  testState.onOpenChange.mockReset();
  testState.initialEnvironmentId = null;
});

afterEach(async () => {
  if (workflowRoot !== null) {
    await act(async () => workflowRoot?.unmount());
  }
  workflowContainer?.remove();
  workflowRoot = null;
  workflowContainer = null;
  document.body.replaceChildren();
  (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = false;
});

describe("useAddProjectWorkflowState", () => {
  it("defaults to the rail-selected host when it is listed", async () => {
    testState.initialEnvironmentId = ENV_REMOTE;

    const view = await mountWorkflow({ open: true });

    expect(view.current.selectedHost.environmentId).toBe(ENV_REMOTE);
  });

  it("falls back to the primary host when the rail selection is not listed", async () => {
    testState.initialEnvironmentId = EnvironmentId.make("unknown");

    const view = await mountWorkflow({ open: true });

    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
  });

  it("selects the primary host and resets it on every open", async () => {
    const view = await mountWorkflow({ open: true });
    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);

    act(() => view.current.selectHost(ENV_REMOTE));
    expect(view.current.selectedHost.environmentId).toBe(ENV_REMOTE);

    await view.rerender(false);
    await view.rerender(true);
    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
    expect(view.current.step).toBe("start");
  });

  it("opens the server directory browser when the selected host is not picker-routable", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    await act(async () => view.current.browse());
    expect(view.current.step).toBe("remote-browse");
    expect(testState.pickFolder).not.toHaveBeenCalled();
  });

  it("adds the browsed folder on the remote host", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    await act(async () => view.current.browse());
    await act(async () => view.current.selectBrowsedFolder("/srv/code/app"));
    expect(testState.operations.addFolder).toHaveBeenCalledWith(
      expect.objectContaining({ environmentId: ENV_REMOTE, workspaceRoot: "/srv/code/app" }),
    );
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("switches from the browser to manual path entry", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    await act(async () => view.current.browse());
    act(() => view.current.openHostPath());
    expect(view.current.step).toBe("host-path");
    expect(view.current.error).toBeNull();
  });

  it("uses the native picker and adds its routed selection", async () => {
    testState.pickResult = {
      _tag: "Selected",
      environmentId: ENV_WSL,
      path: "/home/me/code",
    };
    const view = await mountWorkflow({ open: true });
    await act(async () => view.current.browse());
    expect(testState.operations.addFolder).toHaveBeenCalledWith({
      environmentId: ENV_WSL,
      workspaceRoot: "/home/me/code",
      shouldContinue: expect.any(Function),
    });
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("keeps an unmatched WSL picker failure visible on the launcher", async () => {
    testState.pickResult = {
      _tag: "Failure",
      message: "Start the matching WSL backend, then choose the folder again.",
    };
    const view = await mountWorkflow({ open: true });

    await act(async () => view.current.browse());

    expect(view.current.step).toBe("start");
    expect(view.current.error).toBe(
      "Start the matching WSL backend, then choose the folder again.",
    );
  });

  it("keeps a rejected picker call visible on the launcher", async () => {
    testState.pickFolder.mockRejectedValue(new Error("Native picker crashed."));
    const view = await mountWorkflow({ open: true });

    await act(async () => view.current.browse());

    expect(view.current.step).toBe("start");
    expect(view.current.error).toBe("Native picker crashed.");
  });

  it("resets and invalidates work when the selected host disconnects", async () => {
    const cloneResult = deferredResult<AddProjectOutcome>();
    testState.operations.clone.mockReturnValue(cloneResult.promise);
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    act(() => view.current.setCloneParent("/srv/drafts"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitClone();
    });
    expect(view.current.busy).toBe(true);

    await view.rerender(true, [primaryHost, wslHost]);

    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
    expect(view.current.step).toBe("start");
    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
    expect(view.current.hostPath).toBe("~/");
    expect(view.current.cloneUrl).toBe("");
    expect(view.current.cloneParent).toBe("~/");
    expect(view.current.createName).toBe("");
    expect(view.current.createParent).toBe("~/");
    expect(view.current.error).toBe("The selected host disconnected. Choose a host and try again.");

    cloneResult.resolve(OPENED);
    await act(async () => submission);
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("blocks clone and create until the selected host platform is ready", async () => {
    const notReadyRemote = {
      ...remoteHost,
      platform: null,
    } satisfies AddProjectHostOption;
    const view = await mountWorkflow({ open: true });
    await view.rerender(true, [primaryHost, notReadyRemote, wslHost]);
    act(() => view.current.selectHost(ENV_REMOTE));

    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    await act(async () => view.current.submitClone());
    expect(view.current.error).toBe("Host platform information is still loading.");
    expect(testState.operations.clone).not.toHaveBeenCalled();

    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    await act(async () => view.current.submitCreate());
    expect(view.current.error).toBe("Host platform information is still loading.");
    expect(testState.operations.create).not.toHaveBeenCalled();
  });

  it("ignores stale picker completion after the dialog closes", async () => {
    const pickerResult = deferredResult<PickAddProjectFolderResult>();
    testState.pickFolder.mockReturnValue(pickerResult.promise);
    const view = await mountWorkflow({ open: true });

    act(() => {
      void view.current.browse();
    });
    await view.rerender(false);
    pickerResult.resolve({
      _tag: "Selected",
      environmentId: ENV_PRIMARY,
      path: "/stale",
    });
    await flushPromises();

    expect(testState.operations.addFolder).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("guards host selection while a create is pending", async () => {
    const createResult = deferredResult<boolean>();
    testState.operations.create.mockReturnValue(createResult.promise);
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    act(() => {
      void view.current.submitCreate();
    });

    act(() => view.current.selectHost(ENV_REMOTE));
    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
    expect(view.current.step).toBe("create");
    expect(view.current.busy).toBe(true);

    createResult.resolve(false);
    await flushPromises();

    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
    expect(view.current.step).toBe("create");
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("ignores stale create completion after Back", async () => {
    const createResult = deferredResult<boolean>();
    testState.operations.create.mockReturnValue(createResult.promise);
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    act(() => {
      void view.current.submitCreate();
    });

    act(() => view.current.back());
    createResult.resolve(true);
    await flushPromises();

    expect(view.current.step).toBe("start");
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it.each([
    { invalidation: "close", outcome: "success" },
    { invalidation: "close", outcome: "failure" },
    { invalidation: "host disconnect", outcome: "success" },
    { invalidation: "host disconnect", outcome: "failure" },
    { invalidation: "Back", outcome: "success" },
    { invalidation: "Back", outcome: "failure" },
  ] as const)(
    "stops stale clone side effects after $invalidation on $outcome",
    async ({ invalidation, outcome }) => {
      const harness = makeIntegratedOperations();
      const cloneResult = deferredResult<AddProjectCommandResult<{ readonly path: string }>>();
      harness.cloneRepository.mockReturnValue(cloneResult.promise);
      testState.operationOverride = harness.operations;
      const view = await mountWorkflow({ open: true });
      if (invalidation === "host disconnect") {
        act(() => view.current.selectHost(ENV_REMOTE));
      }
      act(() => view.current.openClone());
      act(() => view.current.setCloneUrl("https://example.test/demo.git"));
      let submission!: Promise<void>;
      act(() => {
        submission = view.current.submitClone();
      });
      expect(harness.cloneRepository).toHaveBeenCalledTimes(1);

      if (invalidation === "close") {
        await view.rerender(false);
      } else if (invalidation === "host disconnect") {
        await view.rerender(true, [primaryHost, wslHost]);
      } else {
        act(() => view.current.back());
      }
      await act(async () => {
        cloneResult.resolve(
          outcome === "success"
            ? {
                _tag: "Success",
                value: { path: "/code/cloned" },
              }
            : {
                _tag: "Failure",
                error: new Error("late clone failure"),
              },
        );
        await submission;
      });

      expect(harness.createProject).not.toHaveBeenCalled();
      expect(harness.reportFailure).not.toHaveBeenCalled();
      expect(harness.openProject).not.toHaveBeenCalled();
      expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
    },
  );

  it.each([{ failure: "registration" }, { failure: "navigation" }] as const)(
    "retries a clone after $failure failure",
    async ({ failure }) => {
      const harness = makeIntegratedOperations();
      const error = new Error(`${failure} unavailable`);
      if (failure === "registration") {
        harness.createProject.mockResolvedValueOnce({
          _tag: "Failure",
          error,
        });
      } else {
        harness.openProject.mockResolvedValueOnce({
          _tag: "Failure",
          error,
        });
      }
      testState.operationOverride = harness.operations;
      const view = await mountWorkflow({ open: true });
      act(() => view.current.openClone());
      act(() => view.current.setCloneUrl("https://example.test/demo.git"));

      await act(async () => view.current.submitClone());
      expect(view.current.step).toBe("clone");
      expect(view.current.busy).toBe(false);
      expect(view.current.error).toBe(
        failure === "registration"
          ? "Failed to add cloned project: registration unavailable"
          : "Failed to open project: navigation unavailable",
      );
      expect(harness.reportFailure).not.toHaveBeenCalled();
      expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);

      await act(async () => view.current.submitClone());

      expect(view.current.error).toBeNull();
      expect(harness.cloneRepository).toHaveBeenCalledTimes(2);
      expect(harness.openProject).toHaveBeenCalledTimes(failure === "registration" ? 1 : 2);
      expect(testState.onOpenChange).toHaveBeenCalledWith(false);
    },
  );

  it.each(["close", "host disconnect", "Back"] as const)(
    "retries a clone after successful disk work is invalidated by %s",
    async (invalidation) => {
      const harness = makeIntegratedOperations();
      const cloneResult = deferredResult<AddProjectCommandResult<{ readonly path: string }>>();
      harness.cloneRepository.mockReturnValueOnce(cloneResult.promise);
      testState.operationOverride = harness.operations;
      const view = await mountWorkflow({ open: true });
      if (invalidation === "host disconnect") {
        act(() => view.current.selectHost(ENV_REMOTE));
      }
      const originalEnvironmentId = view.current.selectedHost.environmentId;
      const originalParent = view.current.cloneParent;
      const url = "https://example.test/demo.git";
      act(() => view.current.openClone());
      act(() => view.current.setCloneUrl(url));
      let staleSubmission!: Promise<void>;
      act(() => {
        staleSubmission = view.current.submitClone();
      });

      if (invalidation === "close") {
        await view.rerender(false);
      } else if (invalidation === "host disconnect") {
        await view.rerender(true, [primaryHost, wslHost]);
      } else {
        act(() => view.current.back());
      }
      await act(async () => {
        cloneResult.resolve({
          _tag: "Success",
          value: { path: "/code/cloned" },
        });
        await staleSubmission;
      });
      expect(harness.createProject).not.toHaveBeenCalled();

      if (invalidation === "close") {
        await view.rerender(true);
      } else if (invalidation === "host disconnect") {
        await view.rerender(true, [primaryHost, remoteHost, wslHost]);
        act(() => view.current.selectHost(originalEnvironmentId));
      }
      act(() => view.current.openClone());
      act(() => view.current.setCloneUrl(url));
      act(() => view.current.setCloneParent(originalParent));
      await act(async () => view.current.submitClone());

      expect(harness.cloneRepository).toHaveBeenCalledTimes(2);
      expect(harness.cloneRepository.mock.calls[1]?.[0]).toMatchObject({
        environmentId: originalEnvironmentId,
        url,
        parentDir: originalParent,
      });
      expect(harness.createProject).toHaveBeenCalledTimes(1);
      expect(harness.openProject).toHaveBeenCalledTimes(1);
      expect(testState.onOpenChange).toHaveBeenCalledWith(false);
    },
  );

  it("does not report or navigate when create fails after close", async () => {
    const harness = makeIntegratedOperations();
    const createResult = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValue(createResult.promise);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitCreate();
    });
    expect(harness.createProject).toHaveBeenCalledTimes(1);

    await view.rerender(false);
    await act(async () => {
      createResult.resolve({
        _tag: "Failure",
        error: new Error("late failure"),
      });
      await submission;
    });

    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("does not navigate or close when add registration completes after Back", async () => {
    const harness = makeIntegratedOperations();
    const createResult = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValue(createResult.promise);
    testState.operationOverride = harness.operations;
    testState.pickResult = {
      _tag: "Selected",
      environmentId: ENV_PRIMARY,
      path: "/code/demo",
    };
    const view = await mountWorkflow({ open: true });
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.browse();
    });
    await flushPromises();
    expect(harness.createProject).toHaveBeenCalledTimes(1);

    act(() => view.current.back());
    await act(async () => {
      const command = harness.createProject.mock.calls[0]?.[0];
      if (command === undefined) throw new Error("Missing project command");
      createResult.resolve({
        _tag: "Success",
        value: { projectId: command.projectId, defaultThreadId: DEFAULT_THREAD_ID },
      });
      await submission;
    });

    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("fresh reopen invalidates an older pending create", async () => {
    const harness = makeIntegratedOperations();
    const createResult = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValue(createResult.promise);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitCreate();
    });

    await view.rerender(false);
    await view.rerender(true);
    await act(async () => {
      const command = harness.createProject.mock.calls[0]?.[0];
      if (command === undefined) throw new Error("Missing project command");
      createResult.resolve({
        _tag: "Success",
        value: { projectId: command.projectId, defaultThreadId: DEFAULT_THREAD_ID },
      });
      await submission;
    });

    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
    expect(view.current.step).toBe("start");
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("lets a current integrated create register, navigate, and close", async () => {
    const harness = makeIntegratedOperations();
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    await act(async () => view.current.submitCreate());

    expect(harness.createProject).toHaveBeenCalledWith(
      expect.objectContaining({
        environmentId: ENV_PRIMARY,
        workspaceRoot: "~/demo",
        createWorkspaceRootIfMissing: true,
        initializeGit: true,
      }),
    );
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).toHaveBeenCalledTimes(1);
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("keeps invalid clone input on the clone step", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("   "));
    await act(async () => view.current.submitClone());

    expect(view.current.step).toBe("clone");
    expect(view.current.error).toBe("Enter a Git URL.");
    expect(testState.operations.clone).not.toHaveBeenCalled();
  });

  it("closes only after a current successful create", async () => {
    testState.operations.create.mockResolvedValue(true);
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    await act(async () => view.current.submitCreate());

    expect(testState.operations.create).toHaveBeenCalledWith({
      environmentId: ENV_PRIMARY,
      workspaceRoot: "~/demo",
      shouldContinue: expect.any(Function),
    });
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("keeps a failed create on the create step for retry", async () => {
    testState.operations.create.mockResolvedValue(false);
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    await act(async () => view.current.submitCreate());

    expect(view.current.step).toBe("create");
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("retargets a clone parent selected through WSL without clearing the URL", async () => {
    testState.pickResult = {
      _tag: "Selected",
      environmentId: ENV_WSL,
      path: "/home/me/code",
    };
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    await act(async () => view.current.pickCloneParent());

    expect(view.current.step).toBe("clone");
    expect(view.current.selectedHost.environmentId).toBe(ENV_WSL);
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(view.current.cloneParent).toBe("/home/me/code");
  });

  it("browses a clone parent on the selected server and clones beneath the chosen folder", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    act(() => view.current.setCloneParent("/srv/existing"));
    await act(async () => view.current.pickCloneParent());

    expect(view.current.step).toBe("clone-parent-browse");
    expect(view.current.selectedHost.environmentId).toBe(ENV_REMOTE);
    expect(view.current.cloneParent).toBe("/srv/existing");
    expect(testState.pickFolder).not.toHaveBeenCalled();

    await act(async () => view.current.selectBrowsedFolder("/srv/chosen"));
    expect(view.current.step).toBe("clone");
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(view.current.cloneParent).toBe("/srv/chosen");
    expect(testState.operations.addFolder).not.toHaveBeenCalled();
    await act(async () => view.current.submitClone());
    expect(testState.operations.clone).toHaveBeenCalledWith(
      expect.objectContaining({
        environmentId: ENV_REMOTE,
        url: "https://example.test/demo.git",
        parentDir: "/srv/chosen",
      }),
    );
  });

  it("browses a new project's parent on the selected server and creates beneath it", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    act(() => view.current.setCreateParent("/srv/existing"));
    await act(async () => view.current.pickCreateParent());

    expect(view.current.step).toBe("create-parent-browse");
    expect(view.current.selectedHost.environmentId).toBe(ENV_REMOTE);
    expect(testState.pickFolder).not.toHaveBeenCalled();

    await act(async () => view.current.selectBrowsedFolder("/srv/chosen"));
    expect(view.current.step).toBe("create");
    expect(view.current.createName).toBe("demo");
    expect(view.current.createParent).toBe("/srv/chosen");
    expect(testState.operations.addFolder).not.toHaveBeenCalled();

    await act(async () => view.current.pickCreateParent());
    act(() => view.current.back());
    expect(view.current.step).toBe("create");
    expect(view.current.createParent).toBe("/srv/chosen");
  });

  it("returns from parent browsing to the clone form without changing its input", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    act(() => view.current.setCloneParent("/srv/entered"));
    await act(async () => view.current.pickCloneParent());
    act(() => view.current.back());

    expect(view.current.step).toBe("clone");
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(view.current.cloneParent).toBe("/srv/entered");
    expect(testState.operations.addFolder).not.toHaveBeenCalled();
    expect(testState.operations.clone).not.toHaveBeenCalled();
  });

  it("ignores a stale parent selection after changing the selected server", async () => {
    const view = await mountWorkflow({ open: true });
    act(() => view.current.selectHost(ENV_REMOTE));
    act(() => view.current.openClone());
    await act(async () => view.current.pickCloneParent());
    const staleSelection = view.current.selectBrowsedFolder;
    act(() => view.current.selectHost(ENV_PRIMARY));
    act(() => view.current.openClone());
    act(() => view.current.setCloneParent("/local/entered"));
    await act(async () => staleSelection("/remote/stale"));

    expect(view.current.selectedHost.environmentId).toBe(ENV_PRIMARY);
    expect(view.current.step).toBe("clone");
    expect(view.current.cloneParent).toBe("/local/entered");
    expect(testState.operations.addFolder).not.toHaveBeenCalled();
  });

  it("browses clone parents for a primary browser client without native dialogs", async () => {
    testState.hosts = [{ ...primaryHost, nativePickerAvailable: false }];
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    await act(async () => view.current.pickCloneParent());

    expect(view.current.step).toBe("clone-parent-browse");
    expect(testState.pickFolder).not.toHaveBeenCalled();
  });

  it("keeps a running clone on the form until Cancel interrupts it", async () => {
    const harness = makeIntegratedOperations();
    const signals = interruptibleClone(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    act(() => view.current.setCloneParent("/code"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitClone();
    });

    expect(view.current.step).toBe("clone");
    expect(view.current.busy).toBe(true);
    expect(view.current.cloneProgress).toBe("cloning");
    expect(signals).toHaveLength(1);
    expect(signals[0]?.aborted).toBe(false);

    await act(async () => {
      view.current.cancelClone();
      await submission;
    });

    expect(signals[0]?.aborted).toBe(true);
    expect(view.current.step).toBe("clone");
    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(view.current.cloneParent).toBe("/code");
    expect(view.current.notice).toBe("Clone cancelled.");
    expect(view.current.error).toBeNull();
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);

    act(() => view.current.setCloneParent("/code/"));
    expect(view.current.notice).toBeNull();
    expect(harness.cancelClone).not.toHaveBeenCalled();
  });

  it("starts a fresh clone after a cancelled one", async () => {
    const harness = makeIntegratedOperations();
    const signals = interruptibleClone(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitClone();
    });
    await act(async () => {
      view.current.cancelClone();
      await submission;
    });

    harness.cloneRepository.mockResolvedValueOnce({
      _tag: "Success",
      value: { path: "/code/demo" },
    });
    await act(async () => view.current.submitClone());

    expect(harness.cloneRepository).toHaveBeenCalledTimes(2);
    expect(harness.cloneRepository.mock.calls[1]?.[0].signal).not.toBe(signals[0]);
    expect(harness.cloneRepository.mock.calls[1]?.[0].signal?.aborted).toBe(false);
    expect(view.current.notice).toBeNull();
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("shows the server's clone failure on the form and keeps Clone available", async () => {
    const harness = makeIntegratedOperations();
    // Test-owned text: the form shows whatever detail the server sends, unchanged.
    const detail = "Synthetic server detail: the destination needs attention.";
    harness.cloneRepository.mockResolvedValueOnce({
      _tag: "Failure",
      error: new GitCommandError({
        operation: "GitVcsDriver.cloneRepository",
        command: "git clone",
        cwd: "/code",
        detail,
      }),
    });
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    act(() => view.current.setCloneParent("/code"));

    await act(async () => view.current.submitClone());

    expect(view.current.error).toBe(`Clone failed: ${detail}`);
    expect(view.current.notice).toBeNull();
    expect(view.current.step).toBe("clone");
    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("names the clone when an interruption the user did not request stops it", async () => {
    const harness = makeIntegratedOperations();
    harness.cloneRepository.mockResolvedValueOnce({ _tag: "Failure", error: null });
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));

    await act(async () => view.current.submitClone());

    expect(view.current.error).toBe("The clone stopped before it finished. Try again.");
    expect(view.current.notice).toBeNull();
    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("names adding the project when an interruption stops registration of a finished clone", async () => {
    const harness = makeIntegratedOperations();
    harness.createProject.mockResolvedValueOnce({ _tag: "Failure", error: null });
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));

    await act(async () => view.current.submitClone());

    expect(harness.cloneRepository).toHaveBeenCalledTimes(1);
    expect(harness.createProject).toHaveBeenCalledTimes(1);
    expect(view.current.error).toBe("Adding the project stopped before it finished. Try again.");
    expect(view.current.notice).toBeNull();
    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
    expect(harness.openProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("closes only after the cloned repository is registered", async () => {
    const harness = makeIntegratedOperations();
    const registration = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValueOnce(registration.promise);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitClone();
    });
    await flushPromises();

    expect(harness.createProject).toHaveBeenCalledTimes(1);
    expect(view.current.cloneProgress).toBe("registering");
    expect(view.current.busy).toBe(true);
    expect(testState.onOpenChange).not.toHaveBeenCalled();

    // The repository is on disk now, so Cancel no longer applies.
    act(() => view.current.cancelClone());
    expect(view.current.cloneProgress).toBe("registering");

    await act(async () => {
      const command = harness.createProject.mock.calls[0]?.[0];
      if (command === undefined) throw new Error("Missing project command");
      registration.resolve({
        _tag: "Success",
        value: { projectId: command.projectId, defaultThreadId: DEFAULT_THREAD_ID },
      });
      await submission;
    });

    expect(harness.openProject).toHaveBeenCalledTimes(1);
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("interrupts a running clone when the workflow unmounts", async () => {
    const harness = makeIntegratedOperations();
    const signals = interruptibleClone(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openClone());
    act(() => view.current.setCloneUrl("https://example.test/demo.git"));
    let submission!: Promise<void>;
    act(() => {
      submission = view.current.submitClone();
    });

    await act(async () => {
      workflowRoot?.unmount();
      await submission;
    });
    workflowRoot = null;

    expect(signals[0]?.aborted).toBe(true);
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
    expect(harness.cancelClone).not.toHaveBeenCalled();
  });
});

const SERVER_CANCELLED = new GitCloneOperationError({
  reason: "cancelled",
  destination: "/srv/code/demo",
  message: "server text",
});

/** A re-attachable clone: reports progress through the captured callback and waits for its reply or an abort. */
function reattachingClone(harness: ReturnType<typeof makeIntegratedOperations>) {
  const calls: Array<Parameters<AddProjectOperationsDependencies["cloneRepository"]>[0]> = [];
  const replies: Array<(result: AddProjectCommandResult<{ readonly path: string }>) => void> = [];
  harness.cloneRepository.mockImplementation(
    (input) =>
      new Promise((resolve) => {
        calls.push(input);
        replies.push(resolve);
        input.onProgress?.({ phase: "cloning", reattach: true });
        input.signal?.addEventListener("abort", () => resolve({ _tag: "Failure", error: null }), {
          once: true,
        });
      }),
  );
  return { calls, replies };
}

/** A server cancel whose settlement the test controls. */
function heldCancel(harness: ReturnType<typeof makeIntegratedOperations>) {
  let settle!: (result: AddProjectCommandResult<{ readonly cancelled: boolean }>) => void;
  harness.cancelClone.mockReturnValueOnce(
    new Promise((resolve) => {
      settle = resolve;
    }),
  );
  return {
    acknowledge: () => settle({ _tag: "Success", value: { cancelled: true } }),
    fail: (error: unknown) => settle({ _tag: "Failure", error }),
  };
}

async function startClone(
  view: Awaited<ReturnType<typeof mountWorkflow>>,
): Promise<{ readonly submission: Promise<void> }> {
  act(() => view.current.selectHost(ENV_REMOTE));
  act(() => view.current.openClone());
  act(() => view.current.setCloneUrl("https://example.test/demo.git"));
  act(() => view.current.setCloneParent("/srv/code"));
  let submission!: Promise<void>;
  act(() => {
    submission = view.current.submitClone();
  });
  await flushPromises();
  // Wrapped: an async function that returned the bare promise would wait for the clone itself.
  return { submission };
}

describe("clone re-attach", () => {
  it("shows the reconnecting line while the connection is lost and registers after re-attach", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);

    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    expect(view.current.cloneProgress).toBe("reconnecting");
    expect(view.current.notice).toBe(
      "Lost the connection to Remote. The clone continues on the server; reconnecting…",
    );
    expect(view.current.busy).toBe(true);

    act(() => clone.calls[0]?.onProgress?.({ phase: "cloning", reattach: true }));
    expect(view.current.cloneProgress).toBe("cloning");
    expect(view.current.notice).toBeNull();

    await act(async () => {
      clone.replies[0]?.({ _tag: "Success", value: { path: "/srv/code/demo" } });
      await submission;
    });
    expect(harness.createProject).toHaveBeenCalledTimes(1);
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("cancels on the server while disconnected and ends with the server's answer", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));

    act(() => view.current.cancelClone());
    expect(view.current.cloneProgress).toBe("cancelling");
    expect(view.current.notice).toBe("The clone stops when Remote reconnects.");
    expect(harness.cancelClone).toHaveBeenCalledWith({
      environmentId: ENV_REMOTE,
      url: "https://example.test/demo.git",
      parentDir: "/srv/code",
    });
    // Progress after Cancel does not reopen Cancel.
    act(() => clone.calls[0]?.onProgress?.({ phase: "cloning", reattach: true }));
    expect(view.current.cloneProgress).toBe("cancelling");

    await act(async () => {
      cancel.acknowledge();
      await Promise.resolve();
    });
    expect(view.current.cloneProgress).toBe("cancelling");
    await act(async () => {
      clone.replies[0]?.({ _tag: "Failure", error: SERVER_CANCELLED });
      await submission;
    });

    expect(clone.calls[0]?.signal?.aborted).toBe(false);
    expect(view.current.notice).toBe("Clone cancelled.");
    expect(view.current.error).toBeNull();
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(view.current.busy).toBe(false);
  });

  it("after Cancel, shows when the clone stops if the connection then drops", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);

    act(() => view.current.cancelClone());
    expect(view.current.cloneProgress).toBe("cancelling");
    expect(view.current.notice).toBeNull();

    // The connection drops before the cancel settles.
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    expect(view.current.cloneProgress).toBe("cancelling");
    expect(view.current.notice).toBe("The clone stops when Remote reconnects.");

    await act(async () => {
      cancel.acknowledge();
      clone.replies[0]?.({ _tag: "Failure", error: SERVER_CANCELLED });
      await submission;
    });
    expect(view.current.notice).toBe("Clone cancelled.");
  });

  it("after a disconnected Cancel, clears the line on reconnect and stays Cancelling… until it settles", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    act(() => view.current.cancelClone());
    expect(view.current.notice).toBe("The clone stops when Remote reconnects.");

    // The host is back while the cancel is still on its way.
    act(() => clone.calls[0]?.onProgress?.({ phase: "cloning", reattach: true }));
    expect(view.current.cloneProgress).toBe("cancelling");
    expect(view.current.notice).toBeNull();
    expect(view.current.busy).toBe(true);

    await act(async () => {
      cancel.acknowledge();
      await Promise.resolve();
    });
    expect(view.current.cloneProgress).toBe("cancelling");
    await act(async () => {
      clone.replies[0]?.({ _tag: "Failure", error: SERVER_CANCELLED });
      await submission;
    });
    expect(view.current.notice).toBe("Clone cancelled.");
    expect(view.current.busy).toBe(false);
  });

  it("keeps Cancelling… until the server cancel settles, so a retry is never cancelled by it", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    act(() => view.current.cancelClone());

    // The clone's own cancelled answer arrives while the cancel's acknowledgement was lost and
    // the cancel is still retrying on the next session.
    await act(async () => {
      clone.replies[0]?.({ _tag: "Failure", error: SERVER_CANCELLED });
      await Promise.resolve();
    });
    expect(view.current.cloneProgress).toBe("cancelling");
    expect(view.current.busy).toBe(true);
    await act(async () => {
      await view.current.submitClone();
    });
    expect(harness.cloneRepository).toHaveBeenCalledTimes(1);

    await act(async () => {
      cancel.acknowledge();
      await submission;
    });
    expect(view.current.notice).toBe("Clone cancelled.");
    expect(view.current.busy).toBe(false);

    // Only now can a retry start, and the settled cancel cannot reach it.
    let retry!: Promise<void>;
    act(() => {
      retry = view.current.submitClone();
    });
    await flushPromises();
    expect(harness.cloneRepository).toHaveBeenCalledTimes(2);
    expect(harness.cancelClone).toHaveBeenCalledTimes(1);
    expect(clone.calls[1]?.signal?.aborted).toBe(false);
    await act(async () => {
      clone.replies[1]?.({ _tag: "Success", value: { path: "/srv/code/demo" } });
      await retry;
    });
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it.each([
    [
      "cancelled elsewhere",
      new GitCloneOperationError({
        reason: "cancelled",
        destination: "/srv/code/demo",
        message: "x",
      }),
      "The clone into /srv/code/demo was cancelled elsewhere. Press Clone to start again.",
    ],
    [
      "not in progress",
      new GitCloneOperationError({
        reason: "not-in-progress",
        destination: "/srv/code/demo",
        message: "x",
      }),
      "No clone is in progress for /srv/code/demo. Press Clone to start again.",
    ],
    [
      "busy",
      new GitCloneOperationError({ reason: "busy", destination: "/srv/code/demo", message: "x" }),
      "Another clone into /srv/code/demo is in progress. Wait for it to finish or choose another folder.",
    ],
    [
      "capacity",
      new GitCloneOperationError({
        reason: "capacity",
        destination: "/srv/code/demo",
        message: "x",
      }),
      "Too many clones are running on Remote. Wait for one to finish and try again.",
    ],
    [
      "shutting down",
      new GitCloneOperationError({
        reason: "shutting-down",
        destination: "/srv/code/demo",
        message: "x",
      }),
      "Remote is shutting down. Press Clone again once it is back.",
    ],
    [
      "unreachable",
      new VcsCloneStoppedError({
        environmentId: "remote",
        reason: "environment-unavailable",
        message: "x",
      }),
      "Can't reconnect to Remote. The clone continues there; clone the same URL into the same folder to finish.",
    ],
  ])("shows the %s copy and keeps the form values", async (_label, failure, copy) => {
    const harness = makeIntegratedOperations();
    harness.cloneRepository.mockResolvedValueOnce({ _tag: "Failure", error: failure });
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    await act(async () => submission);

    expect(view.current.error).toBe(copy);
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(view.current.cloneParent).toBe("/srv/code");
    expect(view.current.cloneProgress).toBe("idle");
    expect(view.current.busy).toBe(false);
  });

  it("shows the can't-reconnect copy when a cancel cannot reach the host", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    act(() => view.current.cancelClone());

    await act(async () => {
      cancel.fail(
        new VcsCloneStoppedError({
          environmentId: "remote",
          reason: "environment-unavailable",
          message: "x",
        }),
      );
      await submission;
    });
    expect(clone.calls[0]?.signal?.aborted).toBe(true);
    expect(view.current.error).toBe(
      "Can't reconnect to Remote. The clone continues there; clone the same URL into the same folder to finish.",
    );
    expect(view.current.busy).toBe(false);
  });

  it("never stays in Cancelling… when the host comes back without re-attach", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    act(() => view.current.cancelClone());

    await act(async () => {
      cancel.fail(
        new VcsCloneStoppedError({
          environmentId: "remote",
          reason: "reattach-unsupported",
          message: "x",
        }),
      );
      await submission;
    });
    expect(view.current.cloneProgress).toBe("idle");
    expect(view.current.busy).toBe(false);
    expect(view.current.error).toBe("The clone stopped before it finished. Try again.");
  });

  it("shows a folder the cancelled clone could not remove instead of a clean cancel", async () => {
    const detail =
      "Git command was interrupted.\nThe incomplete clone at /srv/code/demo could not be removed (the folder was replaced after the clone started). Remove it before trying again.";
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => view.current.cancelClone());

    await act(async () => {
      cancel.acknowledge();
      clone.replies[0]?.({
        _tag: "Failure",
        error: new GitCommandError({
          operation: "GitVcsDriver.clone",
          command: "git",
          cwd: "/srv/code/demo",
          detail,
        }),
      });
      await submission;
    });
    expect(view.current.error).toBe(`Clone failed: ${detail}`);
    expect(view.current.notice).toBeNull();
  });

  it("does not add a clone that finished while its cancel was pending", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => view.current.cancelClone());

    await act(async () => {
      clone.replies[0]?.({ _tag: "Success", value: { path: "/srv/code/demo" } });
      await Promise.resolve();
    });
    expect(harness.createProject).not.toHaveBeenCalled();
    await act(async () => {
      cancel.acknowledge();
      await submission;
    });
    expect(view.current.notice).toBe(
      "The clone finished before it could be cancelled. It is in /srv/code/demo and was not added as a project. Press Clone to add it.",
    );
    expect(view.current.error).toBeNull();
    expect(view.current.cloneUrl).toBe("https://example.test/demo.git");
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("reports a clone that finished before a failed cancel instead of the cancel's error", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => view.current.cancelClone());

    await act(async () => {
      clone.replies[0]?.({ _tag: "Success", value: { path: "/srv/code/demo" } });
      await Promise.resolve();
    });
    await act(async () => {
      cancel.fail(
        new GitCommandError({
          operation: "vcs.cancelClone",
          command: "git",
          cwd: "/srv/code",
          detail: "Synthetic cancel failure.",
        }),
      );
      await submission;
    });
    expect(view.current.notice).toBe(
      "The clone finished before it could be cancelled. It is in /srv/code/demo and was not added as a project. Press Clone to add it.",
    );
    expect(view.current.error).toBeNull();
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(testState.onOpenChange).not.toHaveBeenCalledWith(false);
  });

  it("asks the host to cancel, as best effort, and stops waiting when the workflow unmounts", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);

    await act(async () => {
      workflowRoot?.unmount();
      await submission;
    });
    workflowRoot = null;

    expect(harness.cancelClone).toHaveBeenCalledWith({
      environmentId: ENV_REMOTE,
      url: "https://example.test/demo.git",
      parentDir: "/srv/code",
    });
    expect(clone.calls[0]?.signal?.aborted).toBe(true);
    expect(harness.createProject).not.toHaveBeenCalled();
  });

  it("a stale attempt returns without waiting for its cancel", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    heldCancel(harness); // never settles in this test
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => view.current.cancelClone());

    // The selected host leaves the catalog: the workflow resets, so the attempt is stale.
    await view.rerender(true, [primaryHost, wslHost]);
    await act(async () => {
      clone.replies[0]?.({ _tag: "Failure", error: SERVER_CANCELLED });
      await submission;
    });

    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
    expect(view.current.error).toBe("The selected host disconnected. Choose a host and try again.");
  });
});

describe("closing the dialog while the clone waits for the host", () => {
  it("is dismissible when idle and while waiting for the host, never while cloning", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    expect(view.current.dismissible).toBe(true);

    const { submission } = await startClone(view);
    expect(view.current.cloneProgress).toBe("cloning");
    expect(view.current.dismissible).toBe(false);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    expect(view.current.dismissible).toBe(true);
    act(() => clone.calls[0]?.onProgress?.({ phase: "cloning", reattach: true }));
    expect(view.current.dismissible).toBe(false);
    act(() => view.current.cancelClone());
    expect(view.current.cloneProgress).toBe("cancelling");
    expect(view.current.dismissible).toBe(true);
    // Busy keeps its meaning: Back and host selection stay unavailable.
    expect(view.current.busy).toBe(true);

    await act(async () => {
      cancel.acknowledge();
      clone.replies[0]?.({ _tag: "Failure", error: SERVER_CANCELLED });
      await submission;
    });
    expect(view.current.dismissible).toBe(true);
  });

  it("is not dismissible while the cloned repository is being added", async () => {
    const harness = makeIntegratedOperations();
    const registration = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValueOnce(registration.promise);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    expect(view.current.cloneProgress).toBe("registering");
    expect(view.current.dismissible).toBe(false);

    await act(async () => {
      const command = harness.createProject.mock.calls[0]?.[0];
      if (command === undefined) throw new Error("Missing project command");
      registration.resolve({
        _tag: "Success",
        value: { projectId: command.projectId, defaultThreadId: DEFAULT_THREAD_ID },
      });
      await submission;
    });
    expect(testState.onOpenChange).toHaveBeenCalledWith(false);
  });

  it("is not dismissible while another step is busy", async () => {
    const createResult = deferredResult<boolean>();
    testState.operations.create.mockReturnValue(createResult.promise);
    const view = await mountWorkflow({ open: true });
    act(() => view.current.openCreate());
    act(() => view.current.setCreateName("demo"));
    act(() => {
      void view.current.submitCreate();
    });
    expect(view.current.busy).toBe(true);
    expect(view.current.dismissible).toBe(false);

    createResult.resolve(false);
    await flushPromises();
    expect(view.current.dismissible).toBe(true);
  });

  it("closing while the connection is lost cancels the clone once and stops waiting", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));

    await view.rerender(false);

    expect(harness.cancelClone).toHaveBeenCalledTimes(1);
    expect(harness.cancelClone).toHaveBeenCalledWith({
      environmentId: ENV_REMOTE,
      url: "https://example.test/demo.git",
      parentDir: "/srv/code",
    });
    expect(clone.calls[0]?.signal?.aborted).toBe(true);
    await act(async () => submission);
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(view.current.busy).toBe(false);
    expect(view.current.cloneProgress).toBe("idle");
  });

  it("closing while cancelling sends no second cancel and stops waiting", async () => {
    const harness = makeIntegratedOperations();
    const clone = reattachingClone(harness);
    const cancel = heldCancel(harness);
    testState.operationOverride = harness.operations;
    const view = await mountWorkflow({ open: true });
    const { submission } = await startClone(view);
    act(() => clone.calls[0]?.onProgress?.({ phase: "reconnecting", reattach: true }));
    act(() => view.current.cancelClone());
    expect(harness.cancelClone).toHaveBeenCalledTimes(1);

    await view.rerender(false);

    expect(harness.cancelClone).toHaveBeenCalledTimes(1);
    expect(clone.calls[0]?.signal?.aborted).toBe(true);
    // The cancel still settles on its own, and the closed dialog reports nothing more.
    const noticeWhenClosed = view.current.notice;
    await act(async () => {
      cancel.acknowledge();
      await submission;
    });
    expect(harness.cancelClone).toHaveBeenCalledTimes(1);
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(view.current.notice).toBe(noticeWhenClosed);
    expect(view.current.error).toBeNull();
  });
});
