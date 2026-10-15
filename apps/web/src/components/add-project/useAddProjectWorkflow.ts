"use client";

import type { ConnectionTarget } from "@bibcode/client-runtime/connection";
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import {
  isAtomCommandInterrupted,
  mapAtomCommandResult,
  settlePromise,
  squashAtomCommandFailure,
  type AtomCommandResult,
} from "@bibcode/client-runtime/state/runtime";
import {
  DEFAULT_PROVIDER_INTERACTION_MODE,
  DEFAULT_RUNTIME_MODE,
  DEFAULT_SERVER_SETTINGS,
  EnvironmentId,
  PRIMARY_LOCAL_ENVIRONMENT_ID,
  ProviderInstanceId,
} from "@bibcode/contracts";
import { useNavigate } from "@tanstack/react-router";
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";

import { isDesktopLocalConnectionTarget, desktopLocalBackendId } from "~/connection/desktopLocal";
import { readCurrentEnvironmentPresentationPolicy } from "~/connection/currentEnvironmentPresentation";
import { useDesktopLocalBootstraps } from "~/connection/useDesktopLocalBootstraps";
import { useCenterPanelStore } from "~/centerPanelStore";
import { readLocalApi } from "~/localApi";
import { newThreadId } from "~/lib/utils";
import { useEnvironments, usePrimaryEnvironment } from "~/state/environments";
import {
  readEnvironmentThreadRefs,
  readThreadShell,
  useActiveEnvironmentId,
  useProjects,
} from "~/state/entities";
import { projectEnvironment } from "~/state/projects";
import { threadEnvironment } from "~/state/threads";
import { useAtomCommand } from "~/state/use-atom-command";
import { vcsEnvironment } from "~/state/vcs";
import { resolveProviderSessionSelectionForInstance } from "~/providerSessionSelection";

import { resolveEnvironmentOptionLabel } from "../BranchToolbar.logic";
import {
  buildProviderAgentActions,
  resolveEffectiveProviderAgentAction,
} from "../chat/providerAgentActions";
import { stackedThreadToast, toastManager } from "../ui/toast";
import { buildThreadRouteParams } from "~/threadRoutes";
import {
  CLONE_CANCELLED_NOTICE,
  CLONE_STOPPED_ERROR,
  cloneCancelPendingNotice,
  cloneFinishedBeforeCancelNotice,
  cloneReconnectingNotice,
  defaultAddProjectParent,
  describeCloneOperationFailure,
  getEnvironmentBrowsePlatform,
  joinProjectPath,
  shouldUseNativePicker,
  validateAddProjectPath,
  validateGitCloneParentPath,
  validateGitCloneUrl,
  validateProjectName,
  type AddProjectCloneFeedback,
  type AddProjectCloneProgress,
  type AddProjectHostOption,
  type AddProjectStep,
} from "./AddProjectDialog.logic";
import {
  createAddProjectOperations,
  type AddProjectCommandResult,
  type AddProjectCloneOutcome,
} from "./addProjectOperations";
import { readPrimaryRunningDistro } from "../hostFolderPicker";
import { pickAddProjectFolder, type PickAddProjectFolderResult } from "./pickAddProjectFolder";

export type AddProjectLocationLabel = "Host" | "Location" | null;

export interface AddProjectWorkflow {
  readonly hosts: ReadonlyArray<AddProjectHostOption>;
  readonly locationLabel: AddProjectLocationLabel;
  readonly selectedHost: AddProjectHostOption;
  readonly step: AddProjectStep;
  readonly busy: boolean;
  /**
   * Whether the dialog can be closed: when nothing is running, and while a clone waits for its
   * host (reconnecting or cancelling), which can take as long as the host stays down. Closing
   * then cancels the clone and ends the wait. Never while cloning or adding the project.
   */
  readonly dismissible: boolean;
  readonly cloneProgress: AddProjectCloneProgress;
  readonly hostPath: string;
  readonly cloneUrl: string;
  readonly cloneParent: string;
  readonly createName: string;
  readonly createParent: string;
  readonly error: string | null;
  /** Informational feedback, such as a cancelled clone. Never an error. */
  readonly notice: string | null;
  readonly selectHost: (environmentId: EnvironmentId) => void;
  readonly back: () => void;
  readonly browse: () => Promise<void>;
  readonly setHostPath: (path: string) => void;
  readonly submitHostPath: () => Promise<void>;
  readonly openHostPath: () => void;
  readonly selectBrowsedFolder: (path: string) => Promise<void>;
  readonly openClone: () => void;
  readonly setCloneUrl: (url: string) => void;
  readonly setCloneParent: (path: string) => void;
  readonly pickCloneParent: () => Promise<void>;
  readonly submitClone: () => Promise<void>;
  /** Interrupts a running clone; a clone that is already being registered finishes. */
  readonly cancelClone: () => void;
  readonly openCreate: () => void;
  readonly setCreateName: (name: string) => void;
  readonly setCreateParent: (path: string) => void;
  readonly pickCreateParent: () => Promise<void>;
  readonly submitCreate: () => Promise<void>;
}

export interface AddProjectWorkflowStateInput {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
  readonly hosts: ReadonlyArray<AddProjectHostOption>;
  readonly locationLabel: AddProjectLocationLabel;
  readonly primaryEnvironmentId: EnvironmentId;
  /** Rail-selected environment; wins over the primary host when it is a listed host. */
  readonly initialEnvironmentId: EnvironmentId | null;
  readonly operations: Pick<
    ReturnType<typeof createAddProjectOperations>,
    "addFolder" | "clone" | "create" | "cancelClone"
  >;
  readonly pickFolder: (
    host: AddProjectHostOption,
    initialPath: string,
  ) => Promise<PickAddProjectFolderResult>;
}

function fallbackHost(primaryEnvironmentId: EnvironmentId): AddProjectHostOption {
  return {
    environmentId: primaryEnvironmentId,
    label: "This device",
    platform:
      typeof navigator !== "undefined" && navigator.platform.trim().length > 0
        ? navigator.platform
        : null,
    baseDirectory: "~/",
    isPrimary: true,
    desktopInstanceId: null,
    nativePickerAvailable: typeof window !== "undefined" && window.desktopBridge !== undefined,
  };
}

function initialWorkflowHost(
  input: Pick<
    AddProjectWorkflowStateInput,
    "hosts" | "initialEnvironmentId" | "primaryEnvironmentId"
  >,
): AddProjectHostOption {
  return (
    (input.initialEnvironmentId !== null
      ? input.hosts.find((host) => host.environmentId === input.initialEnvironmentId)
      : undefined) ??
    input.hosts.find((host) => host.environmentId === input.primaryEnvironmentId) ??
    input.hosts[0] ??
    fallbackHost(input.primaryEnvironmentId)
  );
}

function unexpectedErrorMessage(error: unknown): string {
  return error instanceof Error && error.message.trim().length > 0
    ? error.message
    : "An error occurred.";
}

const CLONE_REGISTRATION_STOPPED_ERROR =
  "Adding the project stopped before it finished. Try again.";

/**
 * A Git failure's `detail` is the server's actionable text; its `message` also names the internal
 * operation and working directory.
 */
function addProjectFailureMessage(error: unknown): string {
  if (
    typeof error === "object" &&
    error !== null &&
    "_tag" in error &&
    error._tag === "GitCommandError" &&
    "detail" in error &&
    typeof error.detail === "string" &&
    error.detail.trim().length > 0
  ) {
    return error.detail;
  }
  return unexpectedErrorMessage(error);
}

/** How a server cancel ended. */
type CloneCancellation =
  | { readonly _tag: "Acknowledged"; readonly cancelled: boolean }
  | { readonly _tag: "Failed"; readonly error: unknown };

/** One running clone request. Cleared once the repository is on disk (registration is not cancelled). */
interface CloneAttempt {
  readonly controller: AbortController;
  readonly hostLabel: string;
  readonly requestCancel: () => Promise<AddProjectCommandResult<{ readonly cancelled: boolean }>>;
  phase: "cloning" | "reconnecting";
  /** The server keeps this clone across reconnects: Cancel and close must reach it. */
  reattach: boolean;
  /**
   * The server cancel, once requested. It settles when the host acknowledged it, the
   * environment stopped, or it failed. Until then no new clone may start: the cancel is keyed
   * by destination, so a cancel that is still retrying would stop a new clone into this folder.
   */
  cancellation: Promise<CloneCancellation> | null;
}

function settleCancellation(
  result: Promise<AddProjectCommandResult<{ readonly cancelled: boolean }>>,
): Promise<CloneCancellation> {
  return result.then(
    (settled): CloneCancellation =>
      settled._tag === "Success"
        ? { _tag: "Acknowledged", cancelled: settled.value.cancelled }
        : { _tag: "Failed", error: settled.error },
    (cause: unknown): CloneCancellation => ({ _tag: "Failed", error: cause }),
  );
}

/**
 * Stops a clone attempt whose result nothing can add any more, because the dialog closed or
 * unmounted. With re-attach an abort alone only detaches, so the server cancel goes first unless
 * Cancel already sent it. The cancel is best effort: a closing or disconnected window may never
 * get it out, and then the clone continues on the host. The client runtime keeps the cancel
 * registered until it settles, so a later clone into this folder waits for it.
 */
function stopCloneAttempt(attempt: CloneAttempt | null): void {
  if (attempt === null) return;
  if (attempt.reattach && attempt.cancellation === null) {
    attempt.cancellation = settleCancellation(attempt.requestCancel());
  }
  attempt.controller.abort();
}

function cloneFeedback(
  outcome: Exclude<AddProjectCloneOutcome, { readonly _tag: "Opened" }>,
  attempt: CloneAttempt,
  cancellation: CloneCancellation | null,
  cloned: boolean,
): AddProjectCloneFeedback {
  // Git finished before the Cancel landed: the repository is on disk, left unregistered because
  // the user asked to cancel. Say so, whether the cancel was acknowledged or failed.
  if (outcome._tag === "ClonedNotAdded") {
    return { kind: "notice", text: cloneFinishedBeforeCancelNotice(outcome.path) };
  }
  const cancelRequested = cancellation !== null;
  // The clone's own outcome comes first: a folder a cancelled clone could not remove must show.
  if (outcome._tag === "Failed") {
    return (
      describeCloneOperationFailure(outcome.error, attempt.hostLabel, cancelRequested) ?? {
        kind: "error",
        text: `${outcome.title}: ${addProjectFailureMessage(outcome.error)}`,
      }
    );
  }
  if (cancellation?._tag === "Failed") {
    return (
      describeCloneOperationFailure(cancellation.error, attempt.hostLabel, true) ?? {
        kind: "error",
        text: `Clone failed: ${addProjectFailureMessage(cancellation.error)}`,
      }
    );
  }
  if (cancelRequested || attempt.controller.signal.aborted) {
    return { kind: "notice", text: CLONE_CANCELLED_NOTICE };
  }
  // Interrupted by something other than Cancel; never re-enable the form silently, and name
  // the step that stopped.
  return { kind: "error", text: cloned ? CLONE_REGISTRATION_STOPPED_ERROR : CLONE_STOPPED_ERROR };
}

export function useAddProjectWorkflowState(
  input: AddProjectWorkflowStateInput,
): AddProjectWorkflow {
  const { onOpenChange, pickFolder } = input;
  const initialHost = useMemo(
    () =>
      initialWorkflowHost({
        hosts: input.hosts,
        initialEnvironmentId: input.initialEnvironmentId,
        primaryEnvironmentId: input.primaryEnvironmentId,
      }),
    [input.hosts, input.initialEnvironmentId, input.primaryEnvironmentId],
  );
  const [selectedEnvironmentId, setSelectedEnvironmentId] = useState(initialHost.environmentId);
  const [step, setStep] = useState<AddProjectStep>("start");
  const [parentBrowseGeneration, setParentBrowseGeneration] = useState<number | null>(null);
  const [busy, setBusy] = useState(false);
  const [hostPath, setHostPathState] = useState(initialHost.baseDirectory);
  const [cloneUrl, setCloneUrlState] = useState("");
  const [cloneParent, setCloneParentState] = useState(initialHost.baseDirectory);
  const [createName, setCreateNameState] = useState("");
  const [createParent, setCreateParentState] = useState(initialHost.baseDirectory);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [cloneProgress, setCloneProgress] = useState<AddProjectCloneProgress>("idle");
  const generationRef = useRef(0);
  const busyRef = useRef(false);
  const openRef = useRef(input.open);
  const previousOpenRef = useRef(false);
  /** Set only while the clone request itself runs, so Cancel cannot interrupt registration. */
  const cloneAttemptRef = useRef<CloneAttempt | null>(null);
  useLayoutEffect(() => {
    openRef.current = input.open;
  }, [input.open]);

  const catalogSelectedHost = input.hosts.find(
    (host) => host.environmentId === selectedEnvironmentId,
  );
  const selectedHost = catalogSelectedHost ?? initialHost;

  const resetForHost = useCallback(
    (host: AddProjectHostOption, nextError: string | null = null) => {
      setSelectedEnvironmentId(host.environmentId);
      setStep("start");
      setParentBrowseGeneration(null);
      setBusy(false);
      setCloneProgress("idle");
      setHostPathState(host.baseDirectory);
      setCloneUrlState("");
      setCloneParentState(host.baseDirectory);
      setCreateNameState("");
      setCreateParentState(host.baseDirectory);
      setError(nextError);
      setNotice(null);
    },
    [],
  );

  // Reset the form when the dialog opens and when the selected host leaves the catalog while
  // open. Each committed reset starts a new generation, so earlier work cannot land on it.
  const missingHostFallback = input.open && catalogSelectedHost === undefined ? initialHost : null;
  const [formReset, setFormReset] = useState<{
    readonly open: boolean;
    readonly missingHostFallback: AddProjectHostOption | null;
    readonly count: number;
  }>({ open: false, missingHostFallback: null, count: 0 });
  if (formReset.open !== input.open || formReset.missingHostFallback !== missingHostFallback) {
    let count = formReset.count;
    if (input.open && !formReset.open) {
      resetForHost(initialHost);
      count += 1;
    }
    if (missingHostFallback !== null) {
      resetForHost(
        missingHostFallback,
        "The selected host disconnected. Choose a host and try again.",
      );
      count += 1;
    }
    setFormReset({ open: input.open, missingHostFallback, count });
  }
  const formResetCount = formReset.count;
  useLayoutEffect(() => {
    if (formResetCount === 0) return;
    generationRef.current += 1;
    busyRef.current = false;
  }, [formResetCount]);

  useEffect(() => {
    const wasOpen = previousOpenRef.current;
    previousOpenRef.current = input.open;
    if (!input.open && wasOpen) {
      generationRef.current += 1;
      // Closing while a clone waits for its host cancels it and ends the wait.
      stopCloneAttempt(cloneAttemptRef.current);
      setBusy(false);
      busyRef.current = false;
      setCloneProgress("idle");
    }
  }, [input.open]);

  useEffect(
    () => () => {
      generationRef.current += 1;
      openRef.current = false;
      // Nothing can register the result after unmount, so stop the server-side clone too.
      stopCloneAttempt(cloneAttemptRef.current);
    },
    [],
  );

  const isCurrent = useCallback(
    (generation: number) => generationRef.current === generation && openRef.current,
    [],
  );

  const beginAsync = useCallback((): number | null => {
    if (!openRef.current || busyRef.current) {
      return null;
    }
    busyRef.current = true;
    setBusy(true);
    setError(null);
    setNotice(null);
    return generationRef.current;
  }, []);

  const finishAsync = useCallback(
    (generation: number) => {
      if (!isCurrent(generation)) {
        return;
      }
      busyRef.current = false;
      setBusy(false);
    },
    [isCurrent],
  );

  const closeAfterSuccess = useCallback(
    (generation: number) => {
      if (!isCurrent(generation)) {
        return;
      }
      busyRef.current = false;
      setBusy(false);
      generationRef.current += 1;
      onOpenChange(false);
    },
    [isCurrent, onOpenChange],
  );

  const completeOperation = useCallback(
    async (generation: number, operation: (shouldContinue: () => boolean) => Promise<boolean>) => {
      const shouldContinue = () => isCurrent(generation);
      try {
        const succeeded = await operation(shouldContinue);
        if (!isCurrent(generation)) {
          return;
        }
        if (succeeded) {
          closeAfterSuccess(generation);
        } else {
          finishAsync(generation);
        }
      } catch (cause) {
        if (isCurrent(generation)) {
          finishAsync(generation);
          setError(unexpectedErrorMessage(cause));
        }
      }
    },
    [closeAfterSuccess, finishAsync, isCurrent],
  );

  const selectHost = useCallback(
    (environmentId: EnvironmentId) => {
      if (busyRef.current) {
        return;
      }
      const nextHost = input.hosts.find((host) => host.environmentId === environmentId);
      if (nextHost === undefined || nextHost.environmentId === selectedHost.environmentId) {
        return;
      }
      generationRef.current += 1;
      resetForHost(nextHost);
    },
    [input.hosts, resetForHost, selectedHost.environmentId],
  );

  const back = useCallback(() => {
    generationRef.current += 1;
    busyRef.current = false;
    setBusy(false);
    setCloneProgress("idle");
    setStep((previous) =>
      previous === "clone-parent-browse"
        ? "clone"
        : previous === "create-parent-browse"
          ? "create"
          : "start",
    );
    setParentBrowseGeneration(null);
    setError(null);
    setNotice(null);
  }, []);

  const browse = useCallback(async () => {
    if (!shouldUseNativePicker(selectedHost)) {
      setStep("remote-browse");
      setError(null);
      return;
    }
    const generation = beginAsync();
    if (generation === null) {
      return;
    }
    try {
      const picked = await pickFolder(selectedHost, hostPath);
      if (!isCurrent(generation)) {
        return;
      }
      if (picked._tag === "Cancelled") {
        finishAsync(generation);
        return;
      }
      if (picked._tag === "Failure") {
        finishAsync(generation);
        setError(picked.message);
        return;
      }
      await completeOperation(generation, (shouldContinue) =>
        input.operations.addFolder({
          environmentId: picked.environmentId,
          workspaceRoot: picked.path,
          shouldContinue,
        }),
      );
    } catch (cause) {
      if (isCurrent(generation)) {
        finishAsync(generation);
        setError(unexpectedErrorMessage(cause));
      }
    }
  }, [
    beginAsync,
    completeOperation,
    finishAsync,
    hostPath,
    input.operations,
    isCurrent,
    pickFolder,
    selectedHost,
  ]);

  const submitHostPath = useCallback(async () => {
    const validationError = validateAddProjectPath(hostPath, selectedHost.platform);
    if (validationError !== null) {
      setError(validationError);
      return;
    }
    const generation = beginAsync();
    if (generation === null) {
      return;
    }
    await completeOperation(generation, (shouldContinue) =>
      input.operations.addFolder({
        environmentId: selectedHost.environmentId,
        workspaceRoot: hostPath.trim(),
        shouldContinue,
      }),
    );
  }, [beginAsync, completeOperation, hostPath, input.operations, selectedHost]);

  const openHostPath = useCallback(() => {
    setStep("host-path");
    setError(null);
  }, []);

  const selectBrowsedFolder = useCallback(
    async (path: string) => {
      if (step === "clone-parent-browse" || step === "create-parent-browse") {
        if (
          parentBrowseGeneration === null ||
          !isCurrent(parentBrowseGeneration) ||
          busyRef.current
        ) {
          return;
        }
        generationRef.current += 1;
        setParentBrowseGeneration(null);
        if (step === "clone-parent-browse") {
          setCloneParentState(path);
          setStep("clone");
        } else {
          setCreateParentState(path);
          setStep("create");
        }
        setError(null);
        return;
      }
      const generation = beginAsync();
      if (generation === null) {
        return;
      }
      await completeOperation(generation, (shouldContinue) =>
        input.operations.addFolder({
          environmentId: selectedHost.environmentId,
          workspaceRoot: path,
          shouldContinue,
        }),
      );
    },
    [
      beginAsync,
      completeOperation,
      input.operations,
      isCurrent,
      parentBrowseGeneration,
      selectedHost.environmentId,
      step,
    ],
  );

  const openClone = useCallback(() => {
    setStep("clone");
    setError(null);
    setNotice(null);
  }, []);

  const openCreate = useCallback(() => {
    setStep("create");
    setError(null);
  }, []);

  const pickParent = useCallback(
    async (kind: "clone" | "create") => {
      if (!shouldUseNativePicker(selectedHost)) {
        if (openRef.current && !busyRef.current) {
          setParentBrowseGeneration(generationRef.current);
          setStep(kind === "clone" ? "clone-parent-browse" : "create-parent-browse");
          setError(null);
        }
        return;
      }
      const generation = beginAsync();
      if (generation === null) {
        return;
      }
      const initialPath = kind === "clone" ? cloneParent : createParent;
      try {
        const picked = await pickFolder(selectedHost, initialPath);
        if (!isCurrent(generation)) {
          return;
        }
        if (picked._tag === "Cancelled") {
          finishAsync(generation);
          return;
        }
        if (picked._tag === "Failure") {
          finishAsync(generation);
          setError(picked.message);
          return;
        }
        const routedHost = input.hosts.find((host) => host.environmentId === picked.environmentId);
        if (routedHost === undefined) {
          finishAsync(generation);
          setError("The selected folder host is unavailable.");
          return;
        }
        if (routedHost.environmentId !== selectedHost.environmentId) {
          generationRef.current += 1;
          setSelectedEnvironmentId(routedHost.environmentId);
          setHostPathState(routedHost.baseDirectory);
        }
        busyRef.current = false;
        setBusy(false);
        setError(null);
        if (kind === "clone") {
          setStep("clone");
          setCloneParentState(picked.path);
        } else {
          setStep("create");
          setCreateParentState(picked.path);
        }
      } catch (cause) {
        if (isCurrent(generation)) {
          finishAsync(generation);
          setError(unexpectedErrorMessage(cause));
        }
      }
    },
    [
      beginAsync,
      cloneParent,
      createParent,
      finishAsync,
      input.hosts,
      isCurrent,
      pickFolder,
      selectedHost,
    ],
  );

  const submitClone = useCallback(async () => {
    const validationError =
      validateGitCloneUrl(cloneUrl) ??
      validateGitCloneParentPath(cloneParent, selectedHost.platform);
    if (validationError !== null) {
      setError(validationError);
      return;
    }
    const generation = beginAsync();
    if (generation === null) {
      return;
    }
    const url = cloneUrl.trim();
    const parentDir = cloneParent.trim();
    const environmentId = selectedHost.environmentId;
    const attempt: CloneAttempt = {
      controller: new AbortController(),
      hostLabel: selectedHost.label,
      requestCancel: () => input.operations.cancelClone({ environmentId, url, parentDir }),
      phase: "cloning",
      reattach: false,
      cancellation: null,
    };
    cloneAttemptRef.current = attempt;
    setCloneProgress("cloning");
    let cloned = false;
    let outcome: AddProjectCloneOutcome;
    try {
      outcome = await input.operations.clone({
        environmentId,
        url,
        parentDir,
        shouldContinue: () => isCurrent(generation),
        // After Cancel, a clone that finished anyway stays on disk unregistered.
        shouldRegister: () => attempt.cancellation === null,
        signal: attempt.controller.signal,
        onProgress: (progress) => {
          attempt.phase = progress.phase;
          attempt.reattach = progress.reattach;
          if (!isCurrent(generation)) return;
          if (attempt.cancellation !== null) {
            // Cancel requested: stay Cancelling…, but follow the connection. While it is down,
            // say when the clone stops; once it is back, the cancel goes out and the line clears.
            setNotice(
              progress.phase === "reconnecting"
                ? cloneCancelPendingNotice(attempt.hostLabel)
                : null,
            );
            return;
          }
          setCloneProgress(progress.phase);
          setNotice(
            progress.phase === "reconnecting" ? cloneReconnectingNotice(attempt.hostLabel) : null,
          );
        },
        onCloned: () => {
          cloned = true;
          if (cloneAttemptRef.current === attempt) {
            cloneAttemptRef.current = null;
          }
          if (isCurrent(generation)) {
            setNotice(null);
            setCloneProgress("registering");
          }
        },
      });
    } catch (cause) {
      outcome = { _tag: "Failed", title: "Clone failed", error: cause };
    } finally {
      if (cloneAttemptRef.current === attempt) {
        cloneAttemptRef.current = null;
      }
    }
    // A stale attempt returns at once; the client runtime still holds any new clone into this
    // folder until its cancel has settled.
    if (!isCurrent(generation)) {
      return;
    }
    // Keep Cancelling… until the cancel settles.
    const cancellation = attempt.cancellation === null ? null : await attempt.cancellation;
    if (!isCurrent(generation)) {
      return;
    }
    setCloneProgress("idle");
    setNotice(null);
    if (outcome._tag === "Opened") {
      closeAfterSuccess(generation);
      return;
    }
    finishAsync(generation);
    const feedback = cloneFeedback(outcome, attempt, cancellation, cloned);
    if (feedback.kind === "notice") {
      setNotice(feedback.text);
    } else {
      setError(feedback.text);
    }
  }, [
    beginAsync,
    cloneParent,
    cloneUrl,
    closeAfterSuccess,
    finishAsync,
    input.operations,
    isCurrent,
    selectedHost.environmentId,
    selectedHost.label,
    selectedHost.platform,
  ]);

  const cancelClone = useCallback(() => {
    const attempt = cloneAttemptRef.current;
    if (attempt === null || attempt.cancellation !== null) {
      return;
    }
    if (!attempt.reattach) {
      // Older server: interrupting the request is what stops the clone.
      attempt.controller.abort();
      return;
    }
    setCloneProgress("cancelling");
    setNotice(
      attempt.phase === "reconnecting" ? cloneCancelPendingNotice(attempt.hostLabel) : null,
    );
    const cancellation = settleCancellation(attempt.requestCancel());
    attempt.cancellation = cancellation;
    // A failed or stopped cancel leaves nothing to wait for: stop following the clone. An
    // acknowledged one lets the clone request report its own outcome.
    void cancellation.then((settled) => {
      if (settled._tag === "Failed") {
        attempt.controller.abort();
      }
    });
  }, []);

  const submitCreate = useCallback(async () => {
    const platform = selectedHost.platform;
    const validationError =
      validateProjectName(createName) ?? validateAddProjectPath(createParent, platform);
    if (validationError !== null) {
      setError(validationError);
      return;
    }
    if (platform === null) {
      setError("Host platform information is still loading.");
      return;
    }
    const generation = beginAsync();
    if (generation === null) {
      return;
    }
    await completeOperation(generation, (shouldContinue) =>
      input.operations.create({
        environmentId: selectedHost.environmentId,
        workspaceRoot: joinProjectPath(createParent, createName, platform),
        shouldContinue,
      }),
    );
  }, [beginAsync, completeOperation, createName, createParent, input.operations, selectedHost]);

  const setHostPath = useCallback((path: string) => {
    setHostPathState(path);
    setError(null);
  }, []);
  const setCloneUrl = useCallback((url: string) => {
    setCloneUrlState(url);
    setError(null);
    setNotice(null);
  }, []);
  const setCloneParent = useCallback((path: string) => {
    setCloneParentState(path);
    setError(null);
    setNotice(null);
  }, []);
  const setCreateName = useCallback((name: string) => {
    setCreateNameState(name);
    setError(null);
  }, []);
  const setCreateParent = useCallback((path: string) => {
    setCreateParentState(path);
    setError(null);
  }, []);

  return {
    hosts: input.hosts,
    locationLabel: input.locationLabel,
    selectedHost,
    step,
    busy,
    dismissible: !busy || cloneProgress === "reconnecting" || cloneProgress === "cancelling",
    cloneProgress,
    hostPath,
    cloneUrl,
    cloneParent,
    createName,
    createParent,
    error,
    notice,
    selectHost,
    back,
    browse,
    setHostPath,
    submitHostPath,
    openHostPath,
    selectBrowsedFolder,
    openClone,
    setCloneUrl,
    setCloneParent,
    pickCloneParent: () => pickParent("clone"),
    submitClone,
    cancelClone,
    openCreate,
    setCreateName,
    setCreateParent,
    pickCreateParent: () => pickParent("create"),
    submitCreate,
  };
}

function adaptAtomResult<T, E>(result: AtomCommandResult<T, E>): AddProjectCommandResult<T> {
  if (result._tag === "Success") {
    return { _tag: "Success", value: result.value as T };
  }
  return {
    _tag: "Failure",
    error: isAtomCommandInterrupted(result) ? null : squashAtomCommandFailure(result),
  };
}

/** A stable getter for the latest rendered value, for async work that outlives its render. */
function useLatestGetter<T>(value: T): () => T {
  const ref = useRef(value);
  useLayoutEffect(() => {
    ref.current = value;
  }, [value]);
  return useCallback(() => ref.current, []);
}

/** Saved remote environments (SSH, direct, relay) are add-project hosts. */
function isRemoteEnvironmentTarget(target: ConnectionTarget): boolean {
  return (
    (target._tag === "BearerConnectionTarget" ||
      target._tag === "RelayConnectionTarget" ||
      target._tag === "SshConnectionTarget") &&
    !isDesktopLocalConnectionTarget(target)
  );
}

export function useAddProjectWorkflow(input: {
  readonly open: boolean;
  readonly onOpenChange: (open: boolean) => void;
}): AddProjectWorkflow {
  const { environments } = useEnvironments();
  const primaryEnvironment = usePrimaryEnvironment();
  const activeEnvironmentId = useActiveEnvironmentId();
  const desktopLocalBootstraps = useDesktopLocalBootstraps();
  const projects = useProjects();
  const getProjects = useLatestGetter(projects);
  const navigate = useNavigate();
  const createProject = useAtomCommand(projectEnvironment.create, { reportFailure: false });
  const createDefaultThread = useAtomCommand(threadEnvironment.create, { reportFailure: false });
  const cloneRepository = useAtomCommand(vcsEnvironment.clone, { reportFailure: false });
  const cancelCloneCommand = useAtomCommand(vcsEnvironment.cancelClone, { reportFailure: false });
  const primaryEnvironmentId =
    primaryEnvironment?.environmentId ?? EnvironmentId.make(PRIMARY_LOCAL_ENVIRONMENT_ID);
  const presentation = useMemo(() => readCurrentEnvironmentPresentationPolicy(), []);

  const hosts = useMemo((): ReadonlyArray<AddProjectHostOption> => {
    const remoteEnvironmentIds = new Set(
      environments
        .filter((environment) => isRemoteEnvironmentTarget(environment.entry.target))
        .map((environment) => environment.environmentId),
    );
    const presentedEnvironments = environments.filter(
      (environment) =>
        presentation.presentsTarget(environment.entry.target) ||
        isRemoteEnvironmentTarget(environment.entry.target),
    );
    const catalogHosts = presentedEnvironments.map((environment): AddProjectHostOption => {
      const isPrimary = environment.environmentId === primaryEnvironment?.environmentId;
      const desktopInstanceId = isDesktopLocalConnectionTarget(environment.entry.target)
        ? (desktopLocalBootstraps.find(
            (bootstrap) => bootstrap.httpBaseUrl === environment.displayUrl,
          )?.id ?? null)
        : null;
      return {
        environmentId: environment.environmentId,
        label: resolveEnvironmentOptionLabel({
          isPrimary,
          environmentId: environment.environmentId,
          runtimeLabel: environment.label,
        }),
        platform: getEnvironmentBrowsePlatform(environment.serverConfig?.environment.platform.os),
        baseDirectory: defaultAddProjectParent(
          environment.serverConfig?.settings?.addProjectBaseDirectory,
        ),
        isPrimary,
        desktopInstanceId,
        nativePickerAvailable: typeof window !== "undefined" && window.desktopBridge !== undefined,
      };
    });
    const usableHosts = catalogHosts.filter(
      (host) =>
        presentation.surface === "browser" ||
        host.isPrimary ||
        host.desktopInstanceId !== null ||
        remoteEnvironmentIds.has(host.environmentId),
    );
    return usableHosts.length > 0 ? usableHosts : [fallbackHost(primaryEnvironmentId)];
  }, [
    desktopLocalBootstraps,
    environments,
    presentation,
    primaryEnvironment?.environmentId,
    primaryEnvironmentId,
  ]);
  // The Repositories view has no environment rail, so any choice of host must be offered here.
  const locationLabel: AddProjectLocationLabel =
    presentation.surface === "browser"
      ? "Host"
      : hosts.length <= 1
        ? null
        : presentation.platform === "windows"
          ? "Location"
          : "Host";

  const operations = useMemo(
    () =>
      createAddProjectOperations({
        getProjects,
        createProject: async (commandInput) => {
          const environment = environments.find(
            (candidate) => candidate.environmentId === commandInput.environmentId,
          );
          const serverConfig = environment?.serverConfig;
          const settings = serverConfig?.settings ?? DEFAULT_SERVER_SETTINGS;
          const selectedAction = resolveEffectiveProviderAgentAction(
            buildProviderAgentActions(serverConfig?.providers ?? [], settings),
            settings.defaultAgent,
          );
          const resolution = resolveProviderSessionSelectionForInstance({
            instanceId: selectedAction?.entry.instanceId ?? ProviderInstanceId.make("codex"),
            providers: serverConfig?.providers ?? [],
            settings,
          });
          if (resolution.fallback) {
            console.warn("Provider session default fallback", resolution.fallback);
          }
          return adaptAtomResult(
            mapAtomCommandResult(
              await createProject({
                environmentId: commandInput.environmentId,
                input: {
                  projectId: commandInput.projectId,
                  title: commandInput.title,
                  workspaceRoot: commandInput.workspaceRoot,
                  createWorkspaceRootIfMissing: commandInput.createWorkspaceRootIfMissing,
                  initializeGit: commandInput.initializeGit,
                  defaultModelSelection: resolution.modelSelection,
                },
              }),
              (result) => {
                return {
                  projectId: result.projectId ?? commandInput.projectId,
                  ...(result.threadId ? { defaultThreadId: result.threadId } : {}),
                };
              },
            ),
          );
        },
        cloneRepository: async (commandInput) =>
          adaptAtomResult(
            await cloneRepository(
              {
                environmentId: commandInput.environmentId,
                input: {
                  url: commandInput.url,
                  parentDir: commandInput.parentDir,
                  ...(commandInput.onProgress === undefined
                    ? {}
                    : { onProgress: commandInput.onProgress }),
                },
              },
              // Aborting ends this dialog's wait. Without re-attach it also interrupts the RPC,
              // and the server stops Git; with re-attach only vcs.cancelClone stops the clone.
              { signal: commandInput.signal },
            ),
          ),
        cancelClone: async (commandInput) =>
          adaptAtomResult(
            await cancelCloneCommand({
              environmentId: commandInput.environmentId,
              input: { url: commandInput.url, parentDir: commandInput.parentDir },
            }),
          ),
        openProject: async (commandInput) => {
          let defaultThreadId =
            commandInput.defaultThreadId ??
            readEnvironmentThreadRefs(commandInput.environmentId).find((ref) => {
              const thread = readThreadShell(ref);
              return thread?.projectId === commandInput.projectId && thread.kind === "default";
            })?.threadId;
          const serverConfig = environments.find(
            (candidate) => candidate.environmentId === commandInput.environmentId,
          )?.serverConfig;
          const settings = serverConfig?.settings ?? DEFAULT_SERVER_SETTINGS;
          const selectedAction = resolveEffectiveProviderAgentAction(
            buildProviderAgentActions(serverConfig?.providers ?? [], settings),
            settings.defaultAgent,
          );
          if (!defaultThreadId) {
            defaultThreadId = newThreadId();
            const project = getProjects().find(
              (candidate) =>
                candidate.environmentId === commandInput.environmentId &&
                candidate.id === commandInput.projectId,
            );
            const resolution = resolveProviderSessionSelectionForInstance({
              instanceId: selectedAction?.entry.instanceId ?? ProviderInstanceId.make("codex"),
              providers: serverConfig?.providers ?? [],
              settings,
              projectSelection: project?.defaultModelSelection ?? null,
            });
            if (resolution.fallback) {
              console.warn("Provider session default fallback", resolution.fallback);
            }
            const created = adaptAtomResult(
              mapAtomCommandResult(
                await createDefaultThread({
                  environmentId: commandInput.environmentId,
                  input: {
                    threadId: defaultThreadId,
                    projectId: commandInput.projectId,
                    title: project?.title ?? "Main",
                    modelSelection: resolution.modelSelection,
                    runtimeMode: DEFAULT_RUNTIME_MODE,
                    interactionMode: DEFAULT_PROVIDER_INTERACTION_MODE,
                    branch: null,
                  },
                }),
                () => undefined,
              ),
            );
            if (created._tag === "Failure") {
              return created;
            }
          }
          const threadRef = scopeThreadRef(commandInput.environmentId, defaultThreadId);
          if (
            selectedAction?.kind === "terminal" &&
            selectedAction.terminalAction.command !== null
          ) {
            useCenterPanelStore.getState().replaceMainWithTerminal(threadRef, [], {
              label: selectedAction.terminalAction.label,
              command: selectedAction.terminalAction.command,
            });
          }
          return adaptAtomResult(
            await settlePromise(() =>
              navigate({
                to: "/$environmentId/$threadId",
                params: buildThreadRouteParams(threadRef),
              }),
            ),
          );
        },
        reportFailure: (title, error) => {
          toastManager.add(
            stackedThreadToast({
              type: "error",
              title,
              description: unexpectedErrorMessage(error),
            }),
          );
        },
      }),
    [
      cancelCloneCommand,
      cloneRepository,
      createDefaultThread,
      createProject,
      environments,
      getProjects,
      navigate,
    ],
  );

  const wslCandidates = useMemo(
    () =>
      environments.flatMap((environment) => {
        const backendId = desktopLocalBackendId(environment.entry.target);
        if (backendId === null) {
          return [];
        }
        const bootstrap = desktopLocalBootstraps.find(
          (candidate) => candidate.httpBaseUrl === environment.displayUrl,
        );
        return [
          {
            environmentId: environment.environmentId,
            backendId,
            runningDistro: bootstrap?.runningDistro ?? null,
          },
        ];
      }),
    [desktopLocalBootstraps, environments],
  );

  const pickFolder = useCallback(
    async (
      host: AddProjectHostOption,
      initialPath: string,
    ): Promise<PickAddProjectFolderResult> => {
      const api = readLocalApi();
      if (api === undefined) {
        return { _tag: "Failure", message: "Folder picking is unavailable." };
      }
      return pickAddProjectFolder({
        host,
        primaryEnvironmentId,
        initialPath,
        dialogs: api.dialogs,
        getWslState: () =>
          typeof window === "undefined" || window.desktopBridge === undefined
            ? Promise.resolve(null)
            : window.desktopBridge.getWslState(),
        primaryRunningDistro: readPrimaryRunningDistro(),
        wslCandidates,
      });
    },
    [primaryEnvironmentId, wslCandidates],
  );

  return useAddProjectWorkflowState({
    ...input,
    hosts,
    locationLabel,
    primaryEnvironmentId,
    initialEnvironmentId: activeEnvironmentId,
    operations,
    pickFolder,
  });
}
