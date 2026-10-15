import {
  DEFAULT_PROVIDER_INTERACTION_MODE,
  DEFAULT_RUNTIME_MODE,
  DEFAULT_SERVER_SETTINGS,
  ProviderInstanceId,
  type EnvironmentId,
  type ScopedThreadRef,
  type ServerConfig,
  type VcsWorktreeDescriptor,
  type WorktreeAdoptInput,
} from "@bibcode/contracts";
import { scopeThreadRef } from "@bibcode/client-runtime/environment";
import {
  deriveWorktreeDiscoveryState,
  isWorktreeCatalogSupported,
} from "@bibcode/client-runtime/state/worktrees";
import {
  type AtomCommandResult,
  isAtomCommandInterrupted,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import {
  ChevronRightIcon,
  CloudIcon,
  EyeOffIcon,
  FolderSearchIcon,
  LoaderIcon,
  MonitorIcon,
  PlusIcon,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { newCommandId } from "../lib/utils";
import { resolveProviderSessionSelectionForInstance } from "../providerSessionSelection";
import type { SidebarProjectGroupMember, SidebarProjectSnapshot } from "../sidebarProjectGrouping";
import { useEnvironmentQuery } from "../state/query";
import { useAtomCommand } from "../state/use-atom-command";
import { useWorktreeCatalogFocusRefresh, worktreeEnvironment } from "../state/worktrees";
import {
  buildWorktreeDiscoveryGroups,
  formatDiscoveredWorktreeCount,
  formatWorktreeAddAllSummary,
  type WorktreeDiscoveryCandidatePresentation,
  type WorktreeDiscoveryParentGroup,
} from "./WorktreeDiscoverySection.logic";
import { Button } from "./ui/button";
import { stackedThreadToast, toastManager } from "./ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "./ui/tooltip";

export interface WorktreeDiscoverySectionProps {
  readonly project: SidebarProjectSnapshot;
  readonly serverConfigs: ReadonlyMap<EnvironmentId, ServerConfig>;
  readonly primaryEnvironmentId?: EnvironmentId | null;
  readonly onNavigateToThread: (threadRef: ScopedThreadRef) => void;
  /**
   * Receives the number of hidden discovered worktrees across the project's
   * supported members while the section is mounted and loaded, and null
   * otherwise, so the project menu can label "Show Hidden Worktrees (N)"
   * without starting a query of its own.
   */
  readonly onHiddenCountChange?: (count: number | null) => void;
}

type AtomCommandFailureResult = Extract<
  AtomCommandResult<unknown, unknown>,
  { readonly _tag: "Failure" }
>;

export function getSupportedWorktreeDiscoveryMembers(
  members: ReadonlyArray<SidebarProjectGroupMember>,
  serverConfigs: ReadonlyMap<EnvironmentId, ServerConfig>,
): SidebarProjectGroupMember[] {
  return members.filter((member) => {
    const descriptor = serverConfigs.get(member.environmentId)?.environment;
    return descriptor !== undefined && isWorktreeCatalogSupported(descriptor);
  });
}

function adoptionThreadDefaults(
  member: SidebarProjectGroupMember,
  serverConfig: ServerConfig | undefined,
): WorktreeAdoptInput["threadDefaults"] {
  const settings = serverConfig?.settings ?? DEFAULT_SERVER_SETTINGS;
  const fallbackProvider = serverConfig?.providers.find((provider) => provider.enabled);
  const targetInstanceId =
    member.defaultModelSelection?.instanceId ??
    fallbackProvider?.instanceId ??
    ProviderInstanceId.make("codex");
  const resolution = resolveProviderSessionSelectionForInstance({
    instanceId: targetInstanceId,
    providers: serverConfig?.providers ?? [],
    settings,
    projectSelection: member.defaultModelSelection,
  });
  return {
    modelSelection: resolution.modelSelection,
    runtimeMode: DEFAULT_RUNTIME_MODE,
    interactionMode: DEFAULT_PROVIDER_INTERACTION_MODE,
  };
}

function candidateInput(input: {
  readonly member: SidebarProjectGroupMember;
  readonly candidate: VcsWorktreeDescriptor;
  readonly generation: number;
  readonly serverConfig: ServerConfig | undefined;
}): WorktreeAdoptInput {
  return {
    commandId: newCommandId(),
    projectId: input.member.id,
    worktreeKey: input.candidate.worktreeKey,
    expectedGeneration: input.generation,
    threadDefaults: adoptionThreadDefaults(input.member, input.serverConfig),
  };
}

function EnvironmentBadge(props: {
  readonly environmentLabel: string;
  readonly isRemote: boolean;
}) {
  const { environmentLabel, isRemote } = props;
  const ariaLabel = isRemote
    ? `Remote environment: ${environmentLabel}`
    : `Local environment: ${environmentLabel}`;
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            aria-label={ariaLabel}
            className="inline-flex min-w-0 items-center gap-1 rounded bg-muted px-1.5 py-0.5 text-xs font-medium text-muted-foreground"
          />
        }
      >
        {isRemote ? <CloudIcon className="size-2.5" /> : <MonitorIcon className="size-2.5" />}
        <span className="truncate">{environmentLabel}</span>
      </TooltipTrigger>
      <TooltipPopup side="top">{ariaLabel}</TooltipPopup>
    </Tooltip>
  );
}

function CandidateDetails(props: {
  readonly label: string;
  readonly discriminator: string | null;
  readonly path: string;
  readonly accessiblePhysicalScope: string;
}) {
  const fullPathName = `Full worktree path for ${props.label} in ${props.accessiblePhysicalScope}: ${props.path}`;
  return (
    <Tooltip>
      <TooltipTrigger
        render={
          <span
            aria-label={fullPathName}
            className="flex min-w-0 flex-1 flex-col items-start overflow-hidden rounded-sm text-left outline-hidden focus-visible:ring-1 focus-visible:ring-ring"
            data-worktree-candidate-copy
            tabIndex={0}
          />
        }
      >
        <span className="block max-w-full truncate text-xs font-medium text-foreground/90">
          {props.label}
        </span>
        {props.discriminator ? (
          <span className="block max-w-full truncate font-mono text-xs text-muted-foreground">
            {props.discriminator}
          </span>
        ) : null}
      </TooltipTrigger>
      <TooltipPopup side="top">
        <span className="font-mono">{props.path}</span>
      </TooltipPopup>
    </Tooltip>
  );
}

function CandidateParentGroup(props: {
  readonly accessiblePhysicalScope: string;
  readonly candidateRowClassName: string;
  readonly discoveredBadge?: boolean;
  readonly parentGroup: WorktreeDiscoveryParentGroup;
  readonly renderBeforeAction?: (presentation: WorktreeDiscoveryCandidatePresentation) => ReactNode;
  readonly renderAction: (presentation: WorktreeDiscoveryCandidatePresentation) => ReactNode;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-0.5" data-worktree-parent-group>
      <div
        className="block max-w-full truncate px-1 font-mono text-xs text-muted-foreground"
        data-worktree-parent-directory
      >
        {props.parentGroup.parentDirectory}
      </div>
      {props.parentGroup.candidates.map((presentation) => (
        <div
          key={presentation.candidate.worktreeKey}
          className={props.candidateRowClassName}
          data-worktree-candidate-row
        >
          <CandidateDetails
            accessiblePhysicalScope={props.accessiblePhysicalScope}
            discriminator={presentation.discriminator}
            label={presentation.label}
            path={presentation.candidate.path}
          />
          {props.discoveredBadge ? (
            <span className="shrink-0 rounded bg-info/10 px-1 py-px text-xs font-medium text-info">
              Discovered
            </span>
          ) : null}
          {props.renderBeforeAction?.(presentation)}
          {props.renderAction(presentation)}
        </div>
      ))}
    </div>
  );
}

function PhysicalWorktreeDiscoverySection(props: {
  readonly member: SidebarProjectGroupMember;
  readonly serverConfig: ServerConfig;
  readonly primaryEnvironmentId: EnvironmentId | null;
  readonly onNavigateToThread: (threadRef: ScopedThreadRef) => void;
  readonly onHiddenCountChange: (physicalProjectKey: string, count: number | null) => void;
}) {
  const { member, serverConfig, primaryEnvironmentId, onNavigateToThread, onHiddenCountChange } =
    props;
  const catalog = useEnvironmentQuery(
    worktreeEnvironment.catalog({
      environmentId: member.environmentId,
      input: { projectId: member.id },
    }),
  );
  const updatePolicy = useAtomCommand(worktreeEnvironment.updatePolicy, { reportFailure: false });
  const addOne = useAtomCommand(worktreeEnvironment.addOne, { reportFailure: false });
  const addAll = useAtomCommand(worktreeEnvironment.addAll, { reportFailure: false });
  const [locallyAcknowledgedGeneration, setLocallyAcknowledgedGeneration] = useState<number | null>(
    null,
  );
  const [manuallyExpanded, setManuallyExpanded] = useState(false);
  const [pendingWorktreeKeys, setPendingWorktreeKeys] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const [addAllPendingCount, setAddAllPendingCount] = useState(0);
  const pendingWorktreeKeysRef = useRef(new Set<string>());
  const addAllPendingRef = useRef(false);
  const mountedRef = useRef(false);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const snapshot = catalog.data;
  const discovery = useMemo(
    () =>
      snapshot === null
        ? null
        : deriveWorktreeDiscoveryState({ snapshot, policy: member.worktreeDiscovery }),
    [member.worktreeDiscovery, snapshot],
  );
  const allCandidates = useMemo(
    () =>
      discovery === null ? [] : [...discovery.newCandidates, ...discovery.acknowledgedCandidates],
    [discovery],
  );
  const hiddenCount =
    snapshot === null || discovery === null
      ? null
      : member.worktreeDiscovery.visibility === "hidden"
        ? allCandidates.length
        : 0;
  useEffect(() => {
    onHiddenCountChange(member.physicalProjectKey, hiddenCount);
    return () => onHiddenCountChange(member.physicalProjectKey, null);
  }, [hiddenCount, member.physicalProjectKey, onHiddenCountChange]);
  const initialPromptExpanded =
    snapshot !== null &&
    discovery?.showInitialPrompt === true &&
    locallyAcknowledgedGeneration !== snapshot.generation;
  const cardCandidates =
    initialPromptExpanded && !manuallyExpanded ? (discovery?.newCandidates ?? []) : allCandidates;
  const showDiscoveryCard =
    member.worktreeDiscovery.visibility === "hidden" &&
    (initialPromptExpanded || manuallyExpanded) &&
    cardCandidates.length > 0;
  const showCollapsedLine =
    member.worktreeDiscovery.visibility === "hidden" &&
    allCandidates.length > 0 &&
    !showDiscoveryCard &&
    (discovery?.showCollapsedHiddenLine === true ||
      locallyAcknowledgedGeneration === snapshot?.generation);
  const shownCandidates = discovery?.shownCandidates ?? [];
  const environmentLabel = member.environmentLabel ?? member.environmentId;
  const accessiblePhysicalScope = `${environmentLabel} (environment ${member.environmentId}, project ${member.id})`;
  const isRemote = primaryEnvironmentId !== null && member.environmentId !== primaryEnvironmentId;

  const groupsFor = useCallback(
    (candidates: ReadonlyArray<VcsWorktreeDescriptor>) =>
      buildWorktreeDiscoveryGroups([
        {
          environmentId: member.environmentId,
          environmentLabel,
          projectId: member.id,
          candidates,
        },
      ])[0]?.parentGroups ?? [],
    [environmentLabel, member.environmentId, member.id],
  );

  const reportCommandFailure = useCallback((title: string, result: AtomCommandFailureResult) => {
    if (!mountedRef.current || isAtomCommandInterrupted(result)) return;
    const error = squashAtomCommandFailure(result);
    toastManager.add(
      stackedThreadToast({
        type: "error",
        title,
        description: error instanceof Error ? error.message : "An unexpected error occurred.",
      }),
    );
  }, []);

  const handleAddOne = async (candidate: VcsWorktreeDescriptor) => {
    if (
      snapshot === null ||
      addAllPendingRef.current ||
      pendingWorktreeKeysRef.current.has(candidate.worktreeKey)
    ) {
      return;
    }
    pendingWorktreeKeysRef.current.add(candidate.worktreeKey);
    setPendingWorktreeKeys(new Set(pendingWorktreeKeysRef.current));
    try {
      const result = await addOne({
        environmentId: member.environmentId,
        input: candidateInput({
          member,
          candidate,
          generation: snapshot.generation,
          serverConfig,
        }),
      });
      if (result._tag === "Failure") {
        reportCommandFailure(`Could not add ${candidate.branch ?? "detached worktree"}`, result);
        return;
      }
      if (mountedRef.current) {
        onNavigateToThread(scopeThreadRef(member.environmentId, result.value.threadId));
      }
    } finally {
      pendingWorktreeKeysRef.current.delete(candidate.worktreeKey);
      if (mountedRef.current) {
        setPendingWorktreeKeys(new Set(pendingWorktreeKeysRef.current));
      }
    }
  };

  const handleAddAll = async () => {
    if (snapshot === null || addAllPendingRef.current) return;
    const candidatesToAdd = cardCandidates.filter(
      (candidate) => !pendingWorktreeKeysRef.current.has(candidate.worktreeKey),
    );
    if (candidatesToAdd.length === 0) return;
    addAllPendingRef.current = true;
    setAddAllPendingCount(candidatesToAdd.length);
    try {
      const result = await addAll({
        environmentId: member.environmentId,
        input: {
          candidates: candidatesToAdd.map((candidate) =>
            candidateInput({
              member,
              candidate,
              generation: snapshot.generation,
              serverConfig,
            }),
          ),
        },
      });
      if (result._tag === "Failure") {
        reportCommandFailure("Could not add discovered worktrees", result);
        return;
      }
      const successCount = result.value.results.filter((item) => item._tag === "Success").length;
      if (mountedRef.current) {
        toastManager.add(
          formatWorktreeAddAllSummary({
            successCount,
            failureCount: result.value.results.length - successCount,
          }),
        );
      }
    } finally {
      addAllPendingRef.current = false;
      if (mountedRef.current) {
        setAddAllPendingCount(0);
      }
    }
  };

  const addAllCandidateCount = cardCandidates.filter(
    (candidate) => !pendingWorktreeKeys.has(candidate.worktreeKey),
  ).length;

  const handleKeepHidden = useCallback(async () => {
    if (snapshot === null) return;
    const result = await updatePolicy({
      environmentId: member.environmentId,
      input: {
        commandId: newCommandId(),
        projectId: member.id,
        acknowledgeGeneration: snapshot.generation,
        dismissInitialPrompt: true,
      },
    });
    if (result._tag === "Failure") {
      reportCommandFailure("Could not hide discovered worktrees", result);
      return;
    }
    if (mountedRef.current) {
      setLocallyAcknowledgedGeneration(snapshot.generation);
      setManuallyExpanded(false);
    }
  }, [member.environmentId, member.id, reportCommandFailure, snapshot, updatePolicy]);

  if (snapshot === null || discovery === null || allCandidates.length === 0) {
    return null;
  }

  return (
    <div
      className="flex w-full flex-col gap-1"
      data-environment-id={member.environmentId}
      data-project-id={member.id}
    >
      {showDiscoveryCard ? (
        <div
          className="mx-0.5 rounded-lg border border-border/75 bg-muted/30 p-2 shadow-xs"
          data-testid={`worktree-discovery-card-${member.environmentId}-${member.id}`}
        >
          <div className="mb-2 flex items-start gap-2">
            <FolderSearchIcon className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
            <div className="min-w-0 flex-1">
              <div className="flex items-center justify-between gap-2">
                <span className="text-xs font-medium text-foreground">Discovered worktrees</span>
                <EnvironmentBadge environmentLabel={environmentLabel} isRemote={isRemote} />
              </div>
              <p className="mt-0.5 text-xs text-muted-foreground">
                Add existing Git worktrees to use them in BiBCode.
              </p>
            </div>
          </div>

          <div className="flex flex-col gap-1.5">
            {groupsFor(cardCandidates).map((parentGroup) => (
              <CandidateParentGroup
                key={parentGroup.parentDirectory}
                accessiblePhysicalScope={accessiblePhysicalScope}
                candidateRowClassName="flex min-w-0 items-center gap-1 rounded-md bg-background/65 px-1.5 py-1"
                parentGroup={parentGroup}
                renderAction={({ candidate, label }) => {
                  const pending = pendingWorktreeKeys.has(candidate.worktreeKey);
                  return (
                    <Button
                      aria-label={`Add ${label} from ${accessiblePhysicalScope} at ${candidate.path} to BiBCode`}
                      className="h-5 shrink-0 px-1.5 text-xs"
                      data-worktree-add-action="true"
                      disabled={pending || addAllPendingCount > 0}
                      size="xs"
                      variant="ghost"
                      onClick={() => void handleAddOne(candidate)}
                    >
                      {pending ? <LoaderIcon className="size-3 animate-spin" /> : <PlusIcon />}
                      Add
                    </Button>
                  );
                }}
              />
            ))}
          </div>

          <div className="mt-2 flex items-center justify-end gap-1">
            <Button
              aria-label="Keep hidden"
              className="h-5 px-1.5 text-xs"
              disabled={addAllPendingCount > 0}
              size="xs"
              variant="ghost"
              onClick={() => void handleKeepHidden()}
            >
              Keep hidden
            </Button>
            <Button
              aria-label="Add all discovered worktrees"
              className="h-5 px-1.5 text-xs"
              disabled={addAllPendingCount > 0 || addAllCandidateCount === 0}
              size="xs"
              variant="secondary"
              onClick={() => void handleAddAll()}
            >
              {addAllPendingCount > 0 ? (
                <>
                  <LoaderIcon className="size-3 animate-spin" />
                  Adding {formatDiscoveredWorktreeCount(addAllPendingCount)}…
                </>
              ) : (
                <>Add all</>
              )}
            </Button>
          </div>
        </div>
      ) : null}

      {showCollapsedLine ? (
        <Button
          aria-label={`Hiding ${formatDiscoveredWorktreeCount(allCandidates.length)}`}
          className="h-6 w-full justify-start gap-1.5 px-2 text-xs text-muted-foreground"
          size="xs"
          variant="ghost"
          onClick={() => setManuallyExpanded(true)}
        >
          <EyeOffIcon className="size-3" />
          <span>{`Hiding ${formatDiscoveredWorktreeCount(allCandidates.length)}`}</span>
          <ChevronRightIcon className="ml-auto size-3" />
        </Button>
      ) : null}

      {shownCandidates.length > 0
        ? groupsFor(shownCandidates).map((parentGroup) => (
            <CandidateParentGroup
              key={parentGroup.parentDirectory}
              accessiblePhysicalScope={accessiblePhysicalScope}
              candidateRowClassName="flex min-w-0 items-center gap-1 rounded-md border border-dashed border-info/25 bg-info/5 px-2 py-1.5"
              discoveredBadge
              parentGroup={parentGroup}
              renderBeforeAction={() => (
                <EnvironmentBadge environmentLabel={environmentLabel} isRemote={isRemote} />
              )}
              renderAction={({ candidate, label }) => {
                const pending = pendingWorktreeKeys.has(candidate.worktreeKey);
                return (
                  <Button
                    aria-label={`Add discovered worktree ${label} from ${accessiblePhysicalScope} at ${candidate.path} to BiBCode`}
                    className="h-5 shrink-0 px-1.5 text-xs"
                    data-worktree-add-action="true"
                    disabled={pending || addAllPendingCount > 0}
                    size="xs"
                    variant="ghost"
                    onClick={() => void handleAddOne(candidate)}
                  >
                    {pending ? (
                      <LoaderIcon className="size-3 animate-spin" />
                    ) : (
                      <PlusIcon className="size-3" />
                    )}
                    Add
                  </Button>
                );
              }}
            />
          ))
        : null}
    </div>
  );
}

export function WorktreeDiscoverySection(props: WorktreeDiscoverySectionProps) {
  const {
    project,
    serverConfigs,
    primaryEnvironmentId = null,
    onNavigateToThread,
    onHiddenCountChange,
  } = props;
  const supportedMembers = useMemo(
    () => getSupportedWorktreeDiscoveryMembers(project.memberProjects, serverConfigs),
    [project.memberProjects, serverConfigs],
  );
  const subscribedProjects = useMemo(
    () =>
      supportedMembers.map((member) => ({
        environmentId: member.environmentId,
        projectId: member.id,
      })),
    [supportedMembers],
  );
  useWorktreeCatalogFocusRefresh(subscribedProjects);
  const hiddenCountsRef = useRef(new Map<string, number>());
  const reportMemberHiddenCount = useCallback(
    (physicalProjectKey: string, count: number | null) => {
      const counts = hiddenCountsRef.current;
      if (count === null) {
        counts.delete(physicalProjectKey);
      } else {
        counts.set(physicalProjectKey, count);
      }
      let total = 0;
      for (const member of supportedMembers) {
        const memberCount = counts.get(member.physicalProjectKey);
        if (memberCount === undefined) {
          onHiddenCountChange?.(null);
          return;
        }
        total += memberCount;
      }
      onHiddenCountChange?.(supportedMembers.length === 0 ? null : total);
    },
    [onHiddenCountChange, supportedMembers],
  );

  if (supportedMembers.length === 0) {
    return null;
  }

  return (
    <>
      {supportedMembers.map((member) => (
        <PhysicalWorktreeDiscoverySection
          key={member.physicalProjectKey}
          member={member}
          serverConfig={serverConfigs.get(member.environmentId)!}
          primaryEnvironmentId={primaryEnvironmentId}
          onNavigateToThread={onNavigateToThread}
          onHiddenCountChange={reportMemberHiddenCount}
        />
      ))}
    </>
  );
}
