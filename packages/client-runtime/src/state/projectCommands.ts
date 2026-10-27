import { type EnvironmentId, type ProjectReadFileResult, WS_METHODS } from "@coddy/contracts";
import * as Crypto from "effect/Crypto";
import { Atom } from "effect/reactivity";

import {
  createAtomCommandScheduler,
  createEnvironmentCommand,
  createEnvironmentRpcCommand,
  createEnvironmentRpcQueryAtomFamily,
  createEnvironmentRpcSubscriptionAtomFamily,
} from "./runtime.ts";
import {
  type CreateProjectInput,
  type DeleteProjectInput,
  type UpdateProjectInput,
  createProject,
  deleteProject,
  updateProject,
} from "../operations/commands.ts";
import type { EnvironmentRegistry } from "../connection/registry.ts";

export type {
  CreateProjectInput,
  DeleteProjectInput,
  UpdateProjectInput,
} from "../operations/commands.ts";

export interface OptimisticProjectFile {
  readonly data: ProjectReadFileResult;
  readonly confirmedAgainst: object | null | undefined;
}

export interface OptimisticProjectFileTarget {
  readonly environmentId: EnvironmentId;
  readonly cwd: string;
  readonly relativePath: string;
}

function optimisticProjectFileKey(target: OptimisticProjectFileTarget): string {
  return JSON.stringify([target.environmentId, target.cwd, target.relativePath]);
}

export function createProjectEnvironmentAtoms<R, E>(
  runtime: Atom.AtomRuntime<EnvironmentRegistry | Crypto.Crypto | R, E>,
) {
  const projectScheduler = createAtomCommandScheduler();
  const fileScheduler = createAtomCommandScheduler();
  const optimisticFileFamily = Atom.family((key: string) =>
    Atom.make<OptimisticProjectFile | null>(null).pipe(
      Atom.withLabel(`environment-data:projects:optimistic-file:${key}`),
    ),
  );
  const projectConcurrency = {
    mode: "serial" as const,
    key: ({ environmentId, input }: { environmentId: string; input: { projectId: string } }) =>
      JSON.stringify([environmentId, input.projectId]),
  };
  return {
    searchEntries: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:projects:search-entries",
      tag: WS_METHODS.projectsSearchEntries,
      staleTimeMs: 15_000,
    }),
    /**
     * Signals that the workspace changed on disk outside the application.
     *
     * The stream carries no entry data — the server's index remains the source of truth — so a
     * subscriber refreshes `listEntries` when this fires.
     */
    subscribeEntries: createEnvironmentRpcSubscriptionAtomFamily(runtime, {
      label: "environment-data:projects:subscribe-entries",
      tag: WS_METHODS.projectsSubscribeEntries,
    }),
    listEntries: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:projects:list-entries",
      tag: WS_METHODS.projectsListEntries,
      staleTimeMs: 30_000,
      idleTtlMs: 5 * 60_000,
    }),
    /**
     * The Files panel's full list. The panel mounts it only once `subscribeEntries` has resynced
     * the server's index, and that mount is its read: a list read earlier could predate the resync
     * and would have to be read again. Nothing keeps it once the panel lets go, so every mount is a
     * fresh read rather than a list cached while no panel handled the change signals.
     */
    fileTreeEntries: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:projects:file-tree-entries",
      tag: WS_METHODS.projectsListEntries,
      idleTtlMs: 0,
    }),
    readFile: createEnvironmentRpcQueryAtomFamily(runtime, {
      label: "environment-data:projects:read-file",
      tag: WS_METHODS.projectsReadFile,
      staleTimeMs: 30_000,
      idleTtlMs: 5 * 60_000,
    }),
    optimisticFile: (target: OptimisticProjectFileTarget) =>
      optimisticFileFamily(optimisticProjectFileKey(target)),
    create: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:project:create",
      execute: (input: CreateProjectInput) => createProject(input),
      scheduler: projectScheduler,
      concurrency: projectConcurrency,
    }),
    update: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:project:update",
      execute: (input: UpdateProjectInput) => updateProject(input),
      scheduler: projectScheduler,
      concurrency: projectConcurrency,
    }),
    delete: createEnvironmentCommand(runtime, {
      label: "environment-data:commands:project:delete",
      execute: (input: DeleteProjectInput) => deleteProject(input),
      scheduler: projectScheduler,
      concurrency: projectConcurrency,
    }),
    writeFile: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:write-file",
      tag: WS_METHODS.projectsWriteFile,
      scheduler: fileScheduler,
      concurrency: {
        mode: "serial",
        key: ({ environmentId, input }) =>
          JSON.stringify([environmentId, input.cwd, input.relativePath]),
      },
    }),
    createEntry: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:create-entry",
      tag: WS_METHODS.projectsCreateEntry,
      scheduler: fileScheduler,
      concurrency: {
        mode: "serial",
        key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
      },
    }),
    /** Lists the CLI sessions recorded in the project's folder on the server host. */
    scanAgentSessions: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:scan-agent-sessions",
      tag: WS_METHODS.agentSessionsScan,
    }),
    importAgentSessions: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:commands:project:import-agent-sessions",
      tag: WS_METHODS.agentSessionsImport,
      scheduler: projectScheduler,
      concurrency: projectConcurrency,
    }),
    createDownloadUrl: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:create-download-url",
      tag: WS_METHODS.projectsCreateDownloadUrl,
    }),
    createUploadUrl: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:create-upload-url",
      tag: WS_METHODS.projectsCreateUploadUrl,
    }),
    refreshEntries: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:refresh-entries",
      tag: WS_METHODS.projectsListEntries,
      scheduler: fileScheduler,
      concurrency: {
        mode: "serial",
        key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
      },
    }),
    renameEntry: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:rename-entry",
      tag: WS_METHODS.projectsRenameEntry,
      scheduler: fileScheduler,
      concurrency: {
        mode: "serial",
        key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
      },
    }),
    deleteEntry: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:delete-entry",
      tag: WS_METHODS.projectsDeleteEntry,
      scheduler: fileScheduler,
      concurrency: {
        mode: "serial",
        key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
      },
    }),
    duplicateEntry: createEnvironmentRpcCommand(runtime, {
      label: "environment-data:projects:duplicate-entry",
      tag: WS_METHODS.projectsDuplicateEntry,
      scheduler: fileScheduler,
      concurrency: {
        mode: "serial",
        key: ({ environmentId, input }) => JSON.stringify([environmentId, input.cwd]),
      },
    }),
  };
}
