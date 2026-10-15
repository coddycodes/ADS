import { useAtomValue } from "@effect/atom-react";
import type {
  AuthShareStateResult,
  DesktopBridge,
  DesktopServerExposureMode,
  DesktopServerExposureState,
  EnvironmentId,
} from "@bibcode/contracts";
import { useCallback, useEffect, useLayoutEffect, useRef } from "react";

import { toastManager } from "../components/ui/toast";
import { getServerShareState } from "../environments/primary";
import { authEnvironment } from "./auth";
import { refreshDesktopNetworkAccessState } from "./desktopNetworkAccess";
import { desktopWslStateAtom } from "./desktopWslState";
import { primaryEnvironmentIdAtom } from "./primaryEnvironment";
import { useEnvironmentQuery } from "./query";

export function shouldRevertExposure(input: {
  readonly shareState: AuthShareStateResult;
  readonly exposureMode: DesktopServerExposureMode;
}): boolean {
  return (
    input.exposureMode === "network-accessible" &&
    input.shareState.desiredExposure === "loopback" &&
    input.shareState.legacyGrantCount === 0
  );
}

export interface ShareExposureOperations {
  readonly getShareState: () => Promise<AuthShareStateResult>;
  readonly getExposureState: () => Promise<DesktopServerExposureState>;
  readonly applyExposure: (
    desired: DesktopServerExposureMode,
  ) => Promise<DesktopServerExposureState>;
  readonly canStartExposure: () => boolean;
  readonly operationTimeoutMs?: number;
}

export type ShareExposureReconciliationOutcome = "unchanged" | "narrowed" | "widened" | "rewidened";

export const SHARE_EXPOSURE_BRIDGE_TIMEOUT_MS = 5_000;
const SHARE_EXPOSURE_RECONCILE_RETRY_LIMIT = 3;
const SHARE_EXPOSURE_RECONCILE_RETRY_DELAY_MS = 5_000;

/** One racing primitive shared by every share-exposure deadline helper. */
export async function raceWithDeadline<A>(
  request: Promise<A>,
  timeoutMs: number,
  onTimeout: () => Error,
): Promise<A> {
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  const deadline = new Promise<never>((_resolve, reject) => {
    timeoutId = setTimeout(() => reject(onTimeout()), timeoutMs);
  });
  try {
    return await Promise.race([request, deadline]);
  } finally {
    if (timeoutId !== undefined) clearTimeout(timeoutId);
  }
}

export async function withShareExposureBridgeTimeout<A>(
  operation: string,
  request: Promise<A>,
  timeoutMs = SHARE_EXPOSURE_BRIDGE_TIMEOUT_MS,
): Promise<A> {
  return raceWithDeadline(
    request,
    timeoutMs,
    () => new Error(`${operation} timed out after ${String(timeoutMs)}ms.`),
  );
}

export async function reconcileShareExposureOnce(
  operations: ShareExposureOperations,
): Promise<ShareExposureReconciliationOutcome> {
  const operationTimeoutMs = operations.operationTimeoutMs ?? SHARE_EXPOSURE_BRIDGE_TIMEOUT_MS;
  const [shareState, exposureState] = await Promise.all([
    operations.getShareState(),
    withShareExposureBridgeTimeout(
      "Server exposure state",
      operations.getExposureState(),
      operationTimeoutMs,
    ),
  ]);
  if (shareState.desiredExposure === "wide" && exposureState.mode === "local-only") {
    if (!operations.canStartExposure()) return "unchanged";
    const applied = await withShareExposureBridgeTimeout(
      "Server exposure widening",
      operations.applyExposure("network-accessible"),
      operationTimeoutMs,
    );
    if (applied.mode !== "network-accessible") {
      throw new Error("Server exposure did not reach network-accessible mode.");
    }
    refreshDesktopNetworkAccessState();
    return "widened";
  }
  if (!shouldRevertExposure({ shareState, exposureMode: exposureState.mode })) {
    return "unchanged";
  }

  if (!operations.canStartExposure()) return "unchanged";
  const applied = await withShareExposureBridgeTimeout(
    "Server exposure narrowing",
    operations.applyExposure("local-only"),
    operationTimeoutMs,
  );
  if (applied.mode !== "local-only") {
    throw new Error("Server exposure did not reach local-only mode.");
  }
  refreshDesktopNetworkAccessState();
  const confirmedShareState = await operations.getShareState();
  if (confirmedShareState.desiredExposure !== "wide") return "narrowed";

  const restored = await withShareExposureBridgeTimeout(
    "Server exposure restore",
    operations.applyExposure("network-accessible"),
    operationTimeoutMs,
  );
  if (restored.mode !== "network-accessible") {
    throw new Error("Server exposure did not return to network-accessible mode.");
  }
  refreshDesktopNetworkAccessState();
  return "rewidened";
}

interface ReconcileTarget {
  readonly bridge: DesktopBridge | undefined;
  readonly primaryEnvironmentId: EnvironmentId | null;
  readonly canManageNativeExposure: boolean;
  readonly generation: number;
}

export function useShareExposureReconciler(): void {
  const bridge = typeof window === "undefined" ? undefined : window.desktopBridge;
  const primaryEnvironmentId = useAtomValue(primaryEnvironmentIdAtom);
  const desktopWsl = useEnvironmentQuery(bridge === undefined ? null : desktopWslStateAtom);
  const canManageNativeExposure = desktopWsl.data?.wslOnly === false;
  const canReconcile =
    bridge?.applyServerExposure !== undefined &&
    primaryEnvironmentId !== null &&
    canManageNativeExposure;
  const accessChanges = useEnvironmentQuery(
    canReconcile
      ? authEnvironment.accessChanges({ environmentId: primaryEnvironmentId, input: null })
      : null,
  );
  const revision = accessChanges.data?.revision ?? null;
  // Each committed change of bridge, environment, or native capability starts a
  // new generation, which cancels exposure changes planned for the previous one.
  const targetRef = useRef<ReconcileTarget>({
    bridge,
    primaryEnvironmentId,
    canManageNativeExposure,
    generation: 0,
  });
  useLayoutEffect(() => {
    const previous = targetRef.current;
    if (
      previous.bridge === bridge &&
      previous.primaryEnvironmentId === primaryEnvironmentId &&
      previous.canManageNativeExposure === canManageNativeExposure
    ) {
      return;
    }
    targetRef.current = {
      bridge,
      primaryEnvironmentId,
      canManageNativeExposure,
      generation: previous.generation + 1,
    };
  }, [bridge, canManageNativeExposure, primaryEnvironmentId]);
  const requestedRef = useRef(false);
  const inFlightRef = useRef<Promise<void> | null>(null);
  const mountedRef = useRef(false);
  const failureCountRef = useRef(0);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
      requestedRef.current = false;
      if (retryTimerRef.current !== null) {
        clearTimeout(retryTimerRef.current);
        retryTimerRef.current = null;
      }
    };
  }, []);

  const requestReconcile = useCallback(function requestReconcile() {
    requestedRef.current = true;
    if (inFlightRef.current !== null) return;

    const run = async () => {
      while (requestedRef.current) {
        requestedRef.current = false;
        const target = targetRef.current;
        const targetBridge = target.bridge;
        if (
          targetBridge?.applyServerExposure === undefined ||
          targetBridge.getServerExposureState === undefined ||
          target.primaryEnvironmentId === null ||
          !target.canManageNativeExposure
        ) {
          continue;
        }
        try {
          const outcome = await reconcileShareExposureOnce({
            getShareState: getServerShareState,
            getExposureState: () => targetBridge.getServerExposureState(),
            applyExposure: (desired) => targetBridge.applyServerExposure(desired),
            canStartExposure: () => {
              const current = targetRef.current;
              return (
                mountedRef.current &&
                current.generation === target.generation &&
                current.canManageNativeExposure &&
                current.bridge === targetBridge &&
                current.primaryEnvironmentId === target.primaryEnvironmentId
              );
            },
          });
          if (outcome === "narrowed") {
            toastManager.add({
              type: "info",
              title: "Remote access switched off",
              description:
                "No active off-host pairings remain, so the local server is loopback-only again.",
            });
          }
          failureCountRef.current = 0;
        } catch (error) {
          console.warn("[remote-sharing] Could not reconcile server exposure.", error);
          failureCountRef.current += 1;
          if (failureCountRef.current < SHARE_EXPOSURE_RECONCILE_RETRY_LIMIT) {
            // One failed pass must not abandon convergence until the next
            // authority revision; retry on a bounded schedule.
            if (retryTimerRef.current !== null) clearTimeout(retryTimerRef.current);
            retryTimerRef.current = setTimeout(() => {
              retryTimerRef.current = null;
              if (mountedRef.current && targetRef.current.generation === target.generation) {
                requestReconcile();
              }
            }, SHARE_EXPOSURE_RECONCILE_RETRY_DELAY_MS);
          } else {
            failureCountRef.current = 0;
            toastManager.add({
              type: "warning",
              title: "Remote access state could not be reconciled",
              description:
                "The desktop exposure state could not be brought in line with active pairings. Review Remote servers settings.",
            });
          }
        }
      }
    };

    const inFlight = run().finally(() => {
      if (inFlightRef.current === inFlight) inFlightRef.current = null;
    });
    inFlightRef.current = inFlight;
  }, []);

  // Every authority revision is reconciled while reconciling is possible.
  const reconcileKey = canReconcile ? String(revision) : null;
  useEffect(() => {
    if (reconcileKey === null) return;
    requestReconcile();
  }, [reconcileKey, requestReconcile]);
}
