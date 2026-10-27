import { EnvironmentId, ProjectId, ThreadId } from "@coddy/contracts";
import { describe, expect, it, vi } from "vite-plus/test";

import {
  type AddProjectCommandResult,
  createAddProjectOperations,
  type AddProjectOperationsDependencies,
  type AddProjectRecord,
} from "./addProjectOperations";

const CURRENT_OPERATION = {
  shouldContinue: () => true,
} as const;
const DEFAULT_THREAD_ID = ThreadId.make("default-thread");

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

interface HarnessOptions {
  readonly projects?: ReadonlyArray<AddProjectRecord>;
  readonly clonePath?: string;
  readonly cloneError?: unknown;
  readonly createError?: unknown;
  readonly createInterrupted?: boolean;
}

function makeHarness(options: HarnessOptions = {}) {
  const createProject = vi.fn<AddProjectOperationsDependencies["createProject"]>(async (input) =>
    options.createInterrupted
      ? ({ _tag: "Failure", error: null } as const)
      : options.createError
        ? ({ _tag: "Failure", error: options.createError } as const)
        : ({
            _tag: "Success",
            value: { projectId: input.projectId, defaultThreadId: DEFAULT_THREAD_ID },
          } as const),
  );
  const cloneRepository = vi.fn<AddProjectOperationsDependencies["cloneRepository"]>(async () =>
    options.cloneError
      ? ({ _tag: "Failure", error: options.cloneError } as const)
      : ({
          _tag: "Success" as const,
          value: { path: options.clonePath ?? "/code/cloned" },
        } as const),
  );
  const openProject = vi.fn<AddProjectOperationsDependencies["openProject"]>(async () => ({
    _tag: "Success" as const,
    value: undefined,
  }));
  const cancelClone = vi.fn<AddProjectOperationsDependencies["cancelClone"]>(async () => ({
    _tag: "Success",
    value: { cancelled: true },
  }));
  const reportFailure = vi.fn();
  return {
    createProject,
    cloneRepository,
    openProject,
    cancelClone,
    reportFailure,
    dependencies: {
      getProjects: () => options.projects ?? [],
      createProject,
      cloneRepository,
      cancelClone,
      openProject,
      reportFailure,
    } satisfies AddProjectOperationsDependencies,
  };
}

describe("add project operations", () => {
  it("canonicalizes an existing environment-and-path match without creating a workspace", async () => {
    const existingProjectId = ProjectId.make("existing");
    const canonicalProjectId = ProjectId.make("server-existing");
    const canonicalThreadId = ThreadId.make("server-main");
    const harness = makeHarness({
      projects: [
        {
          id: existingProjectId,
          environmentId: EnvironmentId.make("local"),
          workspaceRoot: "/code/demo",
        },
      ],
    });
    harness.createProject.mockResolvedValue({
      _tag: "Success",
      value: { projectId: canonicalProjectId, defaultThreadId: canonicalThreadId },
    });
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.addFolder({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        workspaceRoot: "/code/demo/",
      }),
    ).resolves.toBe(true);

    expect(harness.createProject).toHaveBeenCalledWith(
      expect.objectContaining({
        environmentId: EnvironmentId.make("local"),
        title: "demo",
        workspaceRoot: "/code/demo/",
        createWorkspaceRootIfMissing: false,
        initializeGit: false,
      }),
    );
    expect(harness.createProject.mock.calls[0]?.[0].projectId).not.toBe(existingProjectId);
    expect(harness.openProject).toHaveBeenCalledWith({
      environmentId: EnvironmentId.make("local"),
      projectId: canonicalProjectId,
      defaultThreadId: canonicalThreadId,
    });
  });

  it("registers an existing folder without creating or initializing it", async () => {
    const harness = makeHarness();
    const operations = createAddProjectOperations(harness.dependencies);

    await operations.addFolder({
      ...CURRENT_OPERATION,
      environmentId: EnvironmentId.make("local"),
      workspaceRoot: "/code/demo",
    });

    expect(harness.createProject).toHaveBeenCalledWith(
      expect.objectContaining({
        createWorkspaceRootIfMissing: false,
        initializeGit: false,
        workspaceRoot: "/code/demo",
      }),
    );
  });

  it("opens the authoritative project identity returned for a canonical duplicate", async () => {
    const harness = makeHarness();
    const authoritativeProjectId = ProjectId.make("server-existing");
    harness.createProject.mockResolvedValue({
      _tag: "Success",
      value: { projectId: authoritativeProjectId, defaultThreadId: DEFAULT_THREAD_ID },
    } as never);
    const operations = createAddProjectOperations(harness.dependencies);

    await operations.addFolder({
      ...CURRENT_OPERATION,
      environmentId: EnvironmentId.make("remote"),
      workspaceRoot: "~/code/demo",
    });

    expect(harness.openProject).toHaveBeenCalledWith({
      environmentId: EnvironmentId.make("remote"),
      projectId: authoritativeProjectId,
      defaultThreadId: DEFAULT_THREAD_ID,
    });
  });

  it("clones before registering the returned path", async () => {
    const harness = makeHarness({ clonePath: "/code/demo" });
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.clone({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        url: "https://example.test/demo.git",
        parentDir: "/code",
      }),
    ).resolves.toEqual({ _tag: "Opened" });

    expect(harness.cloneRepository).toHaveBeenCalledWith({
      environmentId: EnvironmentId.make("local"),
      url: "https://example.test/demo.git",
      parentDir: "/code",
    });
    expect(harness.createProject).toHaveBeenCalledWith(
      expect.objectContaining({
        workspaceRoot: "/code/demo",
        createWorkspaceRootIfMissing: false,
        initializeGit: false,
      }),
    );
    expect(harness.openProject).toHaveBeenCalledTimes(1);
  });

  it("retries clone and registration after registration fails", async () => {
    const harness = makeHarness({ clonePath: "/code/demo" });
    const registrationError = new Error("registration unavailable");
    harness.createProject
      .mockResolvedValueOnce({ _tag: "Failure", error: registrationError })
      .mockImplementation(async (input) => ({
        _tag: "Success",
        value: { projectId: input.projectId, defaultThreadId: DEFAULT_THREAD_ID },
      }));
    const operations = createAddProjectOperations(harness.dependencies);
    const input = {
      ...CURRENT_OPERATION,
      environmentId: EnvironmentId.make("local"),
      url: "https://example.test/demo.git",
      parentDir: "/code",
    };

    await expect(operations.clone(input)).resolves.toEqual({
      _tag: "Failed",
      title: "Failed to add cloned project",
      error: registrationError,
    });
    await expect(operations.clone(input)).resolves.toEqual({ _tag: "Opened" });

    expect(harness.cloneRepository).toHaveBeenCalledTimes(2);
    expect(harness.createProject).toHaveBeenCalledTimes(2);
    expect(harness.openProject).toHaveBeenCalledTimes(1);
    expect(harness.reportFailure).not.toHaveBeenCalled();
  });

  it("retries clone and opens the authoritative project after navigation fails", async () => {
    const harness = makeHarness({ clonePath: "/code/demo" });
    const authoritativeProjectId = ProjectId.make("registered-clone");
    const navigationError = new Error("navigation unavailable");
    harness.createProject.mockResolvedValue({
      _tag: "Success",
      value: { projectId: authoritativeProjectId, defaultThreadId: DEFAULT_THREAD_ID },
    });
    harness.openProject
      .mockResolvedValueOnce({ _tag: "Failure", error: navigationError })
      .mockResolvedValue({ _tag: "Success", value: undefined });
    const operations = createAddProjectOperations(harness.dependencies);
    const input = {
      ...CURRENT_OPERATION,
      environmentId: EnvironmentId.make("local"),
      url: "https://example.test/demo.git",
      parentDir: "/code",
    };

    await expect(operations.clone(input)).resolves.toEqual({
      _tag: "Failed",
      title: "Failed to open project",
      error: navigationError,
    });
    await expect(operations.clone(input)).resolves.toEqual({ _tag: "Opened" });

    expect(harness.cloneRepository).toHaveBeenCalledTimes(2);
    expect(harness.createProject).toHaveBeenCalledTimes(2);
    expect(harness.openProject).toHaveBeenCalledTimes(2);
    expect(harness.openProject).toHaveBeenLastCalledWith({
      environmentId: EnvironmentId.make("local"),
      projectId: authoritativeProjectId,
      defaultThreadId: DEFAULT_THREAD_ID,
    });
    expect(harness.reportFailure).not.toHaveBeenCalled();
  });

  it("returns a failed clone for the form instead of reporting it", async () => {
    const error = new Error("clone denied");
    const harness = makeHarness({ cloneError: error });
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.clone({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        url: "https://example.test/demo.git",
        parentDir: "/code",
      }),
    ).resolves.toEqual({ _tag: "Failed", title: "Clone failed", error });

    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("passes the abort signal to the clone request and stops quietly when interrupted", async () => {
    const harness = makeHarness();
    const controller = new AbortController();
    const onCloned = vi.fn();
    harness.cloneRepository.mockImplementation(
      (input) =>
        new Promise((resolve) => {
          input.signal?.addEventListener("abort", () => resolve({ _tag: "Failure", error: null }), {
            once: true,
          });
        }),
    );
    const operations = createAddProjectOperations(harness.dependencies);

    const result = operations.clone({
      ...CURRENT_OPERATION,
      environmentId: EnvironmentId.make("local"),
      url: "https://example.test/demo.git",
      parentDir: "/code",
      signal: controller.signal,
      onCloned,
    });
    expect(harness.cloneRepository).toHaveBeenCalledWith(
      expect.objectContaining({ signal: controller.signal }),
    );
    controller.abort();

    await expect(result).resolves.toEqual({ _tag: "Stopped" });
    expect(onCloned).not.toHaveBeenCalled();
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("announces a finished clone before registering it", async () => {
    const events: string[] = [];
    const harness = makeHarness({ clonePath: "/code/demo" });
    harness.cloneRepository.mockImplementation(async () => {
      events.push("cloned");
      return { _tag: "Success", value: { path: "/code/demo" } };
    });
    harness.createProject.mockImplementation(async (input) => {
      events.push("registered");
      return {
        _tag: "Success",
        value: { projectId: input.projectId, defaultThreadId: DEFAULT_THREAD_ID },
      };
    });
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.clone({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        url: "https://example.test/demo.git",
        parentDir: "/code",
        onCloned: () => events.push("announced"),
      }),
    ).resolves.toEqual({ _tag: "Opened" });

    expect(events).toEqual(["cloned", "announced", "registered"]);
  });

  it("creates and initializes Git through project.create", async () => {
    const harness = makeHarness();
    const operations = createAddProjectOperations(harness.dependencies);

    await operations.create({
      ...CURRENT_OPERATION,
      environmentId: EnvironmentId.make("local"),
      workspaceRoot: "/code/demo",
    });

    expect(harness.createProject).toHaveBeenCalledWith(
      expect.objectContaining({
        createWorkspaceRootIfMissing: true,
        initializeGit: true,
      }),
    );
  });

  it("reports command failure and does not navigate", async () => {
    const harness = makeHarness({ createError: new Error("disk full") });
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.create({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        workspaceRoot: "/code/demo",
      }),
    ).resolves.toBe(false);

    expect(harness.reportFailure).toHaveBeenCalledWith(
      "Failed to create project",
      new Error("disk full"),
    );
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("suppresses reporting for interrupted commands", async () => {
    const harness = makeHarness({ createInterrupted: true });
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.create({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        workspaceRoot: "/code/demo",
      }),
    ).resolves.toBe(false);

    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("reports a rejected create command and does not navigate", async () => {
    const error = new Error("disk disconnected");
    const harness = makeHarness();
    harness.createProject.mockRejectedValue(error);
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.create({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        workspaceRoot: "/code/demo",
      }),
    ).resolves.toBe(false);

    expect(harness.reportFailure).toHaveBeenCalledWith("Failed to create project", error);
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("returns a rejected clone command as a failure and does not register or navigate", async () => {
    const error = new Error("network disconnected");
    const harness = makeHarness();
    harness.cloneRepository.mockRejectedValue(error);
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.clone({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        url: "https://example.test/demo.git",
        parentDir: "/code",
      }),
    ).resolves.toEqual({ _tag: "Failed", title: "Clone failed", error });

    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("reports a rejected open command", async () => {
    const error = new Error("navigation unavailable");
    const harness = makeHarness({
      projects: [
        {
          id: ProjectId.make("existing"),
          environmentId: EnvironmentId.make("local"),
          workspaceRoot: "/code/demo",
        },
      ],
    });
    harness.openProject.mockRejectedValue(error);
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.addFolder({
        ...CURRENT_OPERATION,
        environmentId: EnvironmentId.make("local"),
        workspaceRoot: "/code/demo",
      }),
    ).resolves.toBe(false);

    expect(harness.createProject).toHaveBeenCalledTimes(1);
    expect(harness.reportFailure).toHaveBeenCalledWith("Failed to open project", error);
  });

  it("does not begin an operation when its continuation is already stale", async () => {
    const harness = makeHarness();
    const operations = createAddProjectOperations(harness.dependencies);

    await expect(
      operations.create({
        environmentId: EnvironmentId.make("local"),
        workspaceRoot: "/code/demo",
        shouldContinue: () => false,
      }),
    ).resolves.toBe(false);

    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("does not register or navigate when a clone becomes stale while pending", async () => {
    const harness = makeHarness();
    const cloneResult = deferredResult<AddProjectCommandResult<{ readonly path: string }>>();
    harness.cloneRepository.mockReturnValue(cloneResult.promise);
    let current = true;
    const operations = createAddProjectOperations(harness.dependencies);

    const result = operations.clone({
      environmentId: EnvironmentId.make("local"),
      url: "https://example.test/demo.git",
      parentDir: "/code",
      shouldContinue: () => current,
    });
    current = false;
    cloneResult.resolve({
      _tag: "Success",
      value: { path: "/code/demo" },
    });

    await expect(result).resolves.toEqual({ _tag: "Stopped" });
    expect(harness.createProject).not.toHaveBeenCalled();
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("does not report a create failure that completes after becoming stale", async () => {
    const harness = makeHarness();
    const createResult = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValue(createResult.promise);
    let current = true;
    const operations = createAddProjectOperations(harness.dependencies);

    const result = operations.create({
      environmentId: EnvironmentId.make("local"),
      workspaceRoot: "/code/demo",
      shouldContinue: () => current,
    });
    current = false;
    createResult.resolve({
      _tag: "Failure",
      error: new Error("late failure"),
    });

    await expect(result).resolves.toBe(false);
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });

  it("does not navigate when add registration completes after becoming stale", async () => {
    const harness = makeHarness();
    const createResult = deferredResult<
      AddProjectCommandResult<{
        readonly projectId: ProjectId;
        readonly defaultThreadId: ThreadId;
      }>
    >();
    harness.createProject.mockReturnValue(createResult.promise);
    let current = true;
    const operations = createAddProjectOperations(harness.dependencies);

    const result = operations.addFolder({
      environmentId: EnvironmentId.make("local"),
      workspaceRoot: "/code/demo",
      shouldContinue: () => current,
    });
    current = false;
    createResult.resolve({
      _tag: "Success",
      value: {
        projectId: ProjectId.make("created"),
        defaultThreadId: DEFAULT_THREAD_ID,
      },
    });

    await expect(result).resolves.toBe(false);
    expect(harness.reportFailure).not.toHaveBeenCalled();
    expect(harness.openProject).not.toHaveBeenCalled();
  });
});

describe("clone re-attach plumbing", () => {
  function operationsWith(
    cloneRepository: AddProjectOperationsDependencies["cloneRepository"],
    createProject: AddProjectOperationsDependencies["createProject"] = vi.fn(async (input) => ({
      _tag: "Success" as const,
      value: { projectId: input.projectId },
    })),
  ) {
    const cancelClone = vi.fn<AddProjectOperationsDependencies["cancelClone"]>(async () => ({
      _tag: "Success",
      value: { cancelled: true },
    }));
    const operations = createAddProjectOperations({
      getProjects: () => [],
      createProject,
      cloneRepository,
      cancelClone,
      openProject: vi.fn(async () => ({ _tag: "Success" as const, value: undefined })),
      reportFailure: vi.fn(),
    });
    return { operations, cancelClone };
  }
  const environmentId = EnvironmentId.make("remote");

  it("forwards progress to the clone command and passes cancel through", async () => {
    const progress = vi.fn();
    const { operations, cancelClone } = operationsWith(async (input) => {
      input.onProgress?.({ phase: "reconnecting", reattach: true });
      return { _tag: "Failure", error: null };
    });

    await operations.clone({
      environmentId,
      url: "https://example.test/demo.git",
      parentDir: "/code",
      shouldContinue: () => true,
      onProgress: progress,
    });
    expect(progress).toHaveBeenCalledWith({ phase: "reconnecting", reattach: true });

    await expect(
      operations.cancelClone({
        environmentId,
        url: "https://example.test/demo.git",
        parentDir: "/code",
      }),
    ).resolves.toEqual({ _tag: "Success", value: { cancelled: true } });
    expect(cancelClone).toHaveBeenCalledWith({
      environmentId,
      url: "https://example.test/demo.git",
      parentDir: "/code",
    });
  });

  it("does not register a finished clone when shouldRegister says no", async () => {
    const createProject = vi.fn<AddProjectOperationsDependencies["createProject"]>();
    const onCloned = vi.fn();
    const { operations } = operationsWith(
      async () => ({ _tag: "Success", value: { path: "/code/demo" } }),
      createProject,
    );

    const outcome = await operations.clone({
      environmentId,
      url: "https://example.test/demo.git",
      parentDir: "/code",
      shouldContinue: () => true,
      shouldRegister: () => false,
      onCloned,
    });

    expect(outcome).toEqual({ _tag: "ClonedNotAdded", path: "/code/demo" });
    expect(onCloned).not.toHaveBeenCalled();
    expect(createProject).not.toHaveBeenCalled();
  });
});
