import type { VcsCloneProgress } from "@coddy/client-runtime/state/vcs";
import type { EnvironmentId, ProjectId, ThreadId } from "@coddy/contracts";

import { findProjectByPath, inferProjectTitleFromPath } from "~/lib/projectPaths";
import { newProjectId } from "~/lib/utils";

export type AddProjectCommandResult<T> =
  | { readonly _tag: "Success"; readonly value: T }
  | { readonly _tag: "Failure"; readonly error: unknown | null };

/**
 * How an add-project operation ended. `Stopped` means it became stale or was interrupted, so
 * there is nothing to report.
 */
export type AddProjectOutcome =
  | { readonly _tag: "Opened" }
  | { readonly _tag: "Failed"; readonly title: string; readonly error: unknown }
  | { readonly _tag: "Stopped" };

/**
 * How a clone ended: an add-project outcome, or `ClonedNotAdded` when the repository reached the
 * disk after Cancel was pressed, so it was left there unregistered.
 */
export type AddProjectCloneOutcome =
  | AddProjectOutcome
  | { readonly _tag: "ClonedNotAdded"; readonly path: string };

type AddProjectCommandOutcome<T> =
  | { readonly _tag: "Success"; readonly value: T }
  | Exclude<AddProjectOutcome, { readonly _tag: "Opened" }>;

const OPENED = { _tag: "Opened" } as const;
const STOPPED = { _tag: "Stopped" } as const;

export interface AddProjectRecord {
  readonly id: ProjectId;
  readonly environmentId: EnvironmentId;
  readonly workspaceRoot: string;
}

export interface AddProjectOperationControl {
  readonly shouldContinue: () => boolean;
}

export interface AddProjectCloneControl extends AddProjectOperationControl {
  /** Aborting interrupts the clone request. Registering a finished clone is not interrupted. */
  readonly signal?: AbortSignal;
  /** Called once the repository is on disk, before it is registered as a project. */
  readonly onCloned?: () => void;
  /** Whether the clone runs or waits for a lost connection, and whether the server re-attaches. */
  readonly onProgress?: (progress: VcsCloneProgress) => void;
  /**
   * Checked once the repository is on disk: `false` leaves it there unregistered (after Cancel),
   * and the clone ends as `ClonedNotAdded` with the repository's path.
   */
  readonly shouldRegister?: () => boolean;
}

export interface AddProjectOperationsDependencies {
  readonly getProjects: () => ReadonlyArray<AddProjectRecord>;
  readonly createProject: (input: {
    readonly environmentId: EnvironmentId;
    readonly projectId: ProjectId;
    readonly title: string;
    readonly workspaceRoot: string;
    readonly createWorkspaceRootIfMissing: boolean;
    readonly initializeGit: boolean;
  }) => Promise<
    AddProjectCommandResult<{
      readonly projectId: ProjectId;
      readonly defaultThreadId?: ThreadId;
    }>
  >;
  readonly cloneRepository: (input: {
    readonly environmentId: EnvironmentId;
    readonly url: string;
    readonly parentDir: string;
    readonly signal?: AbortSignal;
    readonly onProgress?: (progress: VcsCloneProgress) => void;
  }) => Promise<AddProjectCommandResult<{ readonly path: string }>>;
  readonly cancelClone: (input: {
    readonly environmentId: EnvironmentId;
    readonly url: string;
    readonly parentDir: string;
  }) => Promise<AddProjectCommandResult<{ readonly cancelled: boolean }>>;
  readonly openProject: (input: {
    readonly environmentId: EnvironmentId;
    readonly projectId: ProjectId;
    readonly defaultThreadId?: ThreadId;
  }) => Promise<AddProjectCommandResult<void>>;
  readonly reportFailure: (title: string, error: unknown) => void;
}

interface ProjectPathInput extends AddProjectOperationControl {
  readonly environmentId: EnvironmentId;
  readonly workspaceRoot: string;
}

export function createAddProjectOperations(dependencies: AddProjectOperationsDependencies) {
  async function executeCommand<T>(
    failureTitle: string,
    shouldContinue: () => boolean,
    command: () => Promise<AddProjectCommandResult<T>>,
  ): Promise<AddProjectCommandOutcome<T>> {
    if (!shouldContinue()) {
      return STOPPED;
    }
    try {
      const result = await command();
      if (!shouldContinue()) {
        return STOPPED;
      }
      if (result._tag === "Failure") {
        return result.error === null
          ? STOPPED
          : { _tag: "Failed", title: failureTitle, error: result.error };
      }
      return result;
    } catch (error) {
      return shouldContinue()
        ? {
            _tag: "Failed",
            title: failureTitle,
            error: error ?? new Error("Command failed unexpectedly."),
          }
        : STOPPED;
    }
  }

  async function registerOrOpen(
    input: ProjectPathInput & {
      readonly createWorkspaceRootIfMissing: boolean;
      readonly initializeGit: boolean;
      readonly failureTitle: string;
    },
  ): Promise<AddProjectOutcome> {
    if (!input.shouldContinue()) {
      return STOPPED;
    }
    const existing = findProjectByPath(
      dependencies.getProjects().filter((project) => project.environmentId === input.environmentId),
      input.workspaceRoot,
    );
    const created = await executeCommand(input.failureTitle, input.shouldContinue, () =>
      dependencies.createProject({
        environmentId: input.environmentId,
        projectId: newProjectId(),
        title: inferProjectTitleFromPath(input.workspaceRoot),
        workspaceRoot: input.workspaceRoot,
        createWorkspaceRootIfMissing: existing ? false : input.createWorkspaceRootIfMissing,
        initializeGit: existing ? false : input.initializeGit,
      }),
    );
    if (created._tag !== "Success") {
      return created;
    }
    const { projectId, defaultThreadId } = created.value;
    const opened = await executeCommand("Failed to open project", input.shouldContinue, () =>
      dependencies.openProject({
        environmentId: input.environmentId,
        projectId,
        ...(defaultThreadId ? { defaultThreadId } : {}),
      }),
    );
    return opened._tag === "Success" ? OPENED : opened;
  }

  /** Folder and create flows report failures as toasts and close only after opening. */
  function reportOutcome(outcome: AddProjectOutcome, shouldContinue: () => boolean): boolean {
    if (outcome._tag === "Failed" && shouldContinue()) {
      dependencies.reportFailure(outcome.title, outcome.error);
    }
    return outcome._tag === "Opened";
  }

  return {
    addFolder: async (input: ProjectPathInput) =>
      reportOutcome(
        await registerOrOpen({
          ...input,
          createWorkspaceRootIfMissing: false,
          initializeGit: false,
          failureTitle: "Failed to add project",
        }),
        input.shouldContinue,
      ),
    create: async (input: ProjectPathInput) =>
      reportOutcome(
        await registerOrOpen({
          ...input,
          createWorkspaceRootIfMissing: true,
          initializeGit: true,
          failureTitle: "Failed to create project",
        }),
        input.shouldContinue,
      ),
    /** Returns failures instead of reporting them, so the clone form can show them in place. */
    clone: async (
      input: {
        readonly environmentId: EnvironmentId;
        readonly url: string;
        readonly parentDir: string;
      } & AddProjectCloneControl,
    ): Promise<AddProjectCloneOutcome> => {
      const cloned = await executeCommand("Clone failed", input.shouldContinue, () =>
        dependencies.cloneRepository({
          environmentId: input.environmentId,
          url: input.url,
          parentDir: input.parentDir,
          ...(input.signal === undefined ? {} : { signal: input.signal }),
          ...(input.onProgress === undefined ? {} : { onProgress: input.onProgress }),
        }),
      );
      if (cloned._tag !== "Success") {
        return cloned;
      }
      if (input.shouldRegister !== undefined && !input.shouldRegister()) {
        return { _tag: "ClonedNotAdded", path: cloned.value.path };
      }
      input.onCloned?.();
      return registerOrOpen({
        environmentId: input.environmentId,
        workspaceRoot: cloned.value.path,
        shouldContinue: input.shouldContinue,
        createWorkspaceRootIfMissing: false,
        initializeGit: false,
        failureTitle: "Failed to add cloned project",
      });
    },
    /** `vcs.cancelClone`: stops the server-side clone, even across a reconnect. */
    cancelClone: (input: {
      readonly environmentId: EnvironmentId;
      readonly url: string;
      readonly parentDir: string;
    }) => dependencies.cancelClone(input),
  };
}
