"use client";

import { RegistryContext, useAtomSet, useAtomValue } from "@effect/atom-react";
import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import {
  FILL_PREVIEW_VIEWPORT,
  PREVIEW_AUTOMATION_OPERATIONS,
  type EnvironmentId,
  type PreviewAutomationNavigateInput,
  type PreviewAutomationOpenInput,
  type PreviewAutomationResizeInput,
  type PreviewAutomationResizeResult,
  type PreviewAutomationHost as PreviewAutomationHostState,
  type PreviewAutomationRequest,
  type PreviewAutomationStatus,
  type PreviewRenderedViewportSize,
  type PreviewViewportSetting,
  type ScopedThreadRef,
} from "@bibcode/contracts";
import { normalizePreviewUrl } from "@bibcode/shared/preview";
import { resolvePreviewViewport } from "@bibcode/shared/previewViewport";
import { useCallback, useContext, useEffect, useMemo, useState } from "react";
import { Atom } from "effect/unstable/reactivity";

import {
  applyPreviewServerSnapshot,
  readThreadPreviewState,
  reconcilePreviewServerSessions,
  setPreviewLocalFailure,
  updatePreviewServerSnapshot,
} from "~/previewStateStore";
import { supportsPreviewRuntimeCapability } from "~/previewRuntimeCapabilities";
import { useRightPanelStore } from "~/rightPanelStore";
import { resolveBrowserNavigationTarget } from "~/browser/browserTargetResolver";
import {
  canonicalizePreviewUrl,
  type GatewayOpenMutation,
  isGatewayBootstrapUrl,
  resolveForNavigation,
} from "~/browser/previewGateway";
import {
  readActiveBrowserRecordingTabId,
  startBrowserRecording,
  stopBrowserRecording,
} from "~/browser/browserRecording";
import { resolveBrowserRecordingStopTarget } from "~/browser/browserRecordingScope";
import { useBrowserSurfaceStore } from "~/browser/browserSurfaceStore";
import { useEnvironments } from "~/state/environments";
import { previewEnvironment } from "~/state/preview";
import { useAtomQueryRunner } from "~/state/use-atom-query-runner";
import { useAtomCommand } from "~/state/use-atom-command";

import { enqueueOpenPrompt } from "~/browser/openPromptQueue";

import { isBrowserMode, previewBridge } from "./previewBridge";
import {
  PreviewAutomationNavigationTimeoutError,
  PreviewAutomationOperationError,
  PreviewAutomationOverlayTimeoutError,
  PreviewAutomationPageUnreachableError,
  PreviewAutomationRecordingNotActiveError,
  PreviewAutomationTargetUnavailableError,
  PreviewAutomationViewportTimeoutError,
} from "./previewAutomationErrors";
import { previewAutomationOpenNeedsOverlay } from "./previewAutomationOpenReadiness";
import { createPreviewAutomationRequestConsumerAtom } from "./previewAutomationRequestConsumer";
import { createPreviewAutomationClientId } from "./previewAutomationClientId";
import {
  needsPreviewAutomationSessionSync,
  resolvePreviewAutomationOpenTab,
  resolvePreviewAutomationTarget,
} from "./previewAutomationTarget";
import { isPreviewViewportReady } from "./previewViewportReadiness";

/**
 * What a preview bridge without full automation (the Tauri desktop host) can
 * serve: tab status, opening, and navigating. Page reading and input stay with
 * fully automatable bridges, so the server routes them elsewhere or reports
 * that no host can.
 */
const NAVIGATION_ONLY_OPERATIONS = ["status", "open", "navigate"] as const;

/**
 * A plain browser has no internal browser: it reports that, and turns an
 * agent's `open` into a prompt the user answers with a click.
 */
const BROWSER_MODE_OPERATIONS = ["status", "open"] as const;

const NO_AUTOMATION_STATUS: PreviewAutomationStatus = {
  available: false,
  visible: false,
  tabId: null,
  url: null,
  title: null,
  loading: false,
};

const supportedAutomationOperations = () =>
  isBrowserMode()
    ? [...BROWSER_MODE_OPERATIONS]
    : supportsPreviewRuntimeCapability(previewBridge, "automation")
      ? [...PREVIEW_AUTOMATION_OPERATIONS]
      : [...NAVIGATION_ONLY_OPERATIONS];

/**
 * Browser mode: no tab to drive, so `open` asks the user and returns at once;
 * an `open` without an address has nothing to ask about.
 */
const handleBrowserModeRequest = (
  threadRef: ScopedThreadRef,
  request: PreviewAutomationRequest,
): PreviewAutomationStatus | { readonly status: "pending-user" } => {
  const url =
    request.operation === "open" ? (request.input as PreviewAutomationOpenInput).url : undefined;
  if (url === undefined) return NO_AUTOMATION_STATUS;
  // Both throw: an address that is not http(s), or one this client can never reach.
  const canonicalUrl = resolveBrowserNavigationTarget(threadRef.environmentId, {
    kind: "url",
    url: normalizePreviewUrl(url),
  }).resolvedUrl;
  enqueueOpenPrompt({ source: "agent", url: canonicalUrl, threadRef });
  return { status: "pending-user" };
};

const waitForDesktopOverlay = async (
  threadRef: ScopedThreadRef,
  requestId: string,
  tabId: string,
  timeoutMs: number,
): Promise<void> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() <= deadline) {
    const state = readThreadPreviewState(threadRef);
    if (state.desktopByTabId[tabId] && previewBridge) {
      const status = await previewBridge.automation.status(tabId);
      if (status.available) return;
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, 50));
  }
  throw new PreviewAutomationOverlayTimeoutError({
    requestId,
    environmentId: threadRef.environmentId,
    threadId: threadRef.threadId,
    timeoutMs,
  });
};

const waitForNavigationReadiness = async (
  threadRef: ScopedThreadRef,
  requestContext: RequestContext,
  tabId: string,
  readiness: PreviewAutomationNavigateInput["readiness"],
  timeoutMs: number,
): Promise<void> => {
  // Without script evaluation, the page load is the closest observable readiness.
  const targetReadiness =
    readiness === "domContentLoaded" &&
    !supportsPreviewRuntimeCapability(previewBridge, "automation")
      ? "load"
      : (readiness ?? "load");
  if (!previewBridge || targetReadiness === "none") return;
  const deadline = Date.now() + timeoutMs;
  while (Date.now() <= deadline) {
    // A failed load on this client's gateway origin never becomes ready.
    assertNoLocalFailure(threadRef, tabId, requestContext);
    // A gateway bootstrap page loads first and then replaces itself with the
    // real page, so it never counts as ready. Check the raw native URL.
    const status = await previewBridge.automation.status(tabId);
    if (!isGatewayBootstrapUrl(status.url)) {
      if (targetReadiness === "domContentLoaded") {
        const readyState = await previewBridge.automation.evaluate(tabId, {
          expression: "document.readyState",
        });
        if (readyState === "interactive" || readyState === "complete") return;
      } else if (!status.loading) {
        return;
      }
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, 50));
  }
  throw new PreviewAutomationNavigationTimeoutError({
    requestId: requestContext.requestId,
    environmentId: threadRef.environmentId,
    threadId: threadRef.threadId,
    tabId,
    readiness: targetReadiness,
    timeoutMs,
  });
};

interface ExecutablePreviewWebview extends Element {
  readonly executeJavaScript: (code: string, userGesture?: boolean) => Promise<unknown>;
}

const findPreviewWebview = (tabId: string): ExecutablePreviewWebview | null =>
  Array.from(document.querySelectorAll<ExecutablePreviewWebview>("webview[data-preview-tab]")).find(
    (candidate) => candidate.getAttribute("data-preview-tab") === tabId,
  ) ?? null;

const readWebviewViewport = async (
  webview: ExecutablePreviewWebview,
): Promise<PreviewRenderedViewportSize | null> => {
  const value = await webview.executeJavaScript(
    "({ width: window.innerWidth, height: window.innerHeight })",
  );
  if (typeof value !== "object" || value === null) return null;
  const { width, height } = value as { readonly width?: unknown; readonly height?: unknown };
  return typeof width === "number" &&
    Number.isInteger(width) &&
    width > 0 &&
    typeof height === "number" &&
    Number.isInteger(height) &&
    height > 0
    ? { width, height }
    : null;
};

const readRenderedViewport = async (tabId: string): Promise<PreviewRenderedViewportSize | null> => {
  const webview = findPreviewWebview(tabId);
  if (!webview) return null;
  return await readWebviewViewport(webview);
};

const readDeclaredViewport = (
  webview: ExecutablePreviewWebview | null,
): PreviewRenderedViewportSize | null => {
  const width = Number(webview?.getAttribute("data-preview-css-width"));
  const height = Number(webview?.getAttribute("data-preview-css-height"));
  return Number.isInteger(width) && width > 0 && Number.isInteger(height) && height > 0
    ? { width, height }
    : null;
};

const waitForRenderedViewport = async (
  tabId: string,
  setting: PreviewViewportSetting,
  timeoutMs: number,
  context: {
    readonly requestId: PreviewAutomationRequest["requestId"];
    readonly environmentId: EnvironmentId;
    readonly threadId: PreviewAutomationRequest["threadId"];
  },
): Promise<PreviewRenderedViewportSize> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() <= deadline) {
    try {
      const webview = findPreviewWebview(tabId);
      const appliedSettingKey = webview?.getAttribute("data-preview-viewport-key") ?? null;
      const declaredViewport = readDeclaredViewport(webview);
      const renderedViewport = webview ? await readWebviewViewport(webview) : null;
      if (
        renderedViewport &&
        isPreviewViewportReady({
          setting,
          appliedSettingKey,
          declaredViewport,
          renderedViewport,
        })
      ) {
        return renderedViewport;
      }
    } catch {
      // Registration and navigation can transiently replace the guest while
      // React applies the server snapshot. Retry until the operation deadline.
    }
    await new Promise<void>((resolve) => window.setTimeout(resolve, 50));
  }
  throw new PreviewAutomationViewportTimeoutError({
    ...context,
    tabId,
    timeoutMs,
  });
};

const currentStatus = async (
  threadRef: ScopedThreadRef,
  requestedTabId: string | null,
): Promise<PreviewAutomationStatus> => {
  const state = readThreadPreviewState(threadRef);
  const { snapshot, tabId } = resolvePreviewAutomationTarget(state, requestedTabId);
  const visible = tabId
    ? (useBrowserSurfaceStore.getState().byTabId[tabId]?.visible ?? false)
    : false;
  const viewportSetting = snapshot ? (snapshot.viewport ?? FILL_PREVIEW_VIEWPORT) : undefined;
  const viewport = tabId ? await readRenderedViewport(tabId).catch(() => null) : null;
  const viewportStatus = {
    ...(viewportSetting === undefined ? {} : { viewportSetting }),
    ...(viewport === null ? {} : { viewport }),
  };
  if (tabId && previewBridge && state.desktopByTabId[tabId]) {
    const status = await previewBridge.automation.status(tabId);
    // A detached native tab keeps its last overlay; its session is the truth then.
    if (status.available) {
      // The webview may sit on this client's gateway origin; agents see the canonical URL.
      const url = status.url === null ? null : canonicalizePreviewUrl(status.url, tabId);
      return { ...status, url, visible, ...viewportStatus };
    }
  }
  const navStatus = snapshot?.navStatus;
  return {
    available: Boolean(previewBridge?.automation),
    visible,
    tabId,
    url: navStatus && navStatus._tag !== "Idle" ? navStatus.url : null,
    title: navStatus && navStatus._tag !== "Idle" ? navStatus.title : null,
    loading: navStatus?._tag === "Loading",
    ...viewportStatus,
  };
};

type RequestContext = Omit<
  ConstructorParameters<typeof PreviewAutomationPageUnreachableError>[0],
  "reason" | "tabId"
>;

const requestContext = (
  environmentId: EnvironmentId,
  request: PreviewAutomationRequest,
): RequestContext => ({
  requestId: request.requestId,
  operation: request.operation,
  environmentId,
  threadId: request.threadId,
});

/** Throws the reason this client couldn't load the tab's page, if it couldn't. */
const assertNoLocalFailure = (
  threadRef: ScopedThreadRef,
  tabId: string | null,
  context: RequestContext,
): void => {
  const failure = tabId ? readThreadPreviewState(threadRef).localFailures[tabId] : undefined;
  if (failure === undefined) return;
  throw new PreviewAutomationPageUnreachableError({
    ...context,
    tabId,
    reason: failure.code === 0 ? failure.description : `${failure.description} (${failure.code})`,
  });
};

/**
 * A fresh tab's native host resolves its URL after `open` returns. Waits until
 * the first page starts loading, and fails with the reason when it can't.
 */
const waitForFirstNavigation = async (
  threadRef: ScopedThreadRef,
  tabId: string,
  timeoutMs: number,
  context: RequestContext,
): Promise<void> => {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() <= deadline) {
    assertNoLocalFailure(threadRef, tabId, context);
    const state = readThreadPreviewState(threadRef);
    const navStatus = state.sessions[tabId]?.navStatus;
    if (navStatus?._tag === "LoadFailed") {
      throw new PreviewAutomationPageUnreachableError({
        ...context,
        tabId,
        reason: navStatus.description,
      });
    }
    if ((state.desktopByTabId[tabId]?.url ?? null) !== null) return;
    await new Promise<void>((resolve) => window.setTimeout(resolve, 50));
  }
  throw new PreviewAutomationNavigationTimeoutError({
    requestId: context.requestId,
    environmentId: context.environmentId,
    threadId: context.threadId,
    tabId,
    readiness: "load",
    timeoutMs,
  });
};

/** Resolves a canonical URL for this client's native view, failing with the user-facing reason. */
const resolveNativeUrl = async (
  threadRef: ScopedThreadRef,
  tabId: string,
  canonicalUrl: string,
  gatewayOpen: GatewayOpenMutation,
  context: RequestContext,
): Promise<string> => {
  const target = await resolveForNavigation({
    environmentId: threadRef.environmentId,
    threadId: threadRef.threadId,
    canonicalUrl,
    gatewayOpen,
    tabId,
  });
  if (target.kind === "unreachable") {
    throw new PreviewAutomationPageUnreachableError({ ...context, tabId, reason: target.message });
  }
  // An earlier failure must not fail the navigation that is about to start.
  setPreviewLocalFailure(threadRef, tabId, null);
  return target.url;
};

const raiseAtomCommandFailure = (result: Parameters<typeof squashAtomCommandFailure>[0]): never => {
  throw squashAtomCommandFailure(result);
};

const raisePreviewAutomationHostError = (
  error: PreviewAutomationRecordingNotActiveError,
): never => {
  throw error;
};

export function PreviewAutomationHosts() {
  const { environments } = useEnvironments();
  // A desktop bridge without an automation surface hosts nothing; no bridge is browser mode.
  if (!isBrowserMode() && !previewBridge?.automation) return null;
  return (
    <>
      {/*
       * Host lifetime follows the desktop runtime's environment connections,
       * not the routed thread. This keeps background threads automatable and
       * lets the subscription runtime own reconnects for every saved target.
       */}
      {environments.map((environment) => (
        <PreviewAutomationHost
          key={environment.environmentId}
          environmentId={environment.environmentId}
        />
      ))}
    </>
  );
}

function PreviewAutomationHost(props: { readonly environmentId: EnvironmentId }) {
  const { environmentId } = props;
  const registry = useContext(RegistryContext);
  const [automationClientId] = useState(createPreviewAutomationClientId);
  const initialAutomationHost = useMemo<PreviewAutomationHostState>(
    () => ({
      clientId: automationClientId,
      environmentId,
      supportedOperations: supportedAutomationOperations(),
    }),
    [automationClientId, environmentId],
  );
  const automationRequestsAtom = previewEnvironment.automationRequests({
    environmentId,
    input: initialAutomationHost,
  });
  const listPreviews = useAtomQueryRunner(previewEnvironment.list, {
    reportFailure: false,
  });
  const open = useAtomCommand(previewEnvironment.open, {
    reportFailure: false,
  });
  const resize = useAtomCommand(previewEnvironment.resize, {
    reportFailure: false,
  });
  const gatewayOpen = useAtomCommand(previewEnvironment.gatewayOpen, {
    reportFailure: false,
  });
  const respondToAutomation = useAtomCommand(
    previewEnvironment.respondToAutomation,
    "preview automation response",
  );
  const focusAutomationHost = useAtomCommand(
    previewEnvironment.focusAutomationHost,
    "preview automation host focus",
  );
  const [automationConnectionAtom] = useState(() => Atom.make<string | null>(null));
  const automationConnectionId = useAtomValue(automationConnectionAtom);

  const handleRequest = useCallback(
    async (request: PreviewAutomationRequest): Promise<unknown> => {
      const threadRef: ScopedThreadRef = {
        environmentId,
        threadId: request.threadId,
      };
      let tabId = request.tabId ?? null;
      try {
        if (isBrowserMode() && (request.operation === "status" || request.operation === "open")) {
          return handleBrowserModeRequest(threadRef, request);
        }
        let state = readThreadPreviewState(threadRef);
        const needsSessionSync = needsPreviewAutomationSessionSync(state, request.tabId);
        if (needsSessionSync) {
          const listTarget = {
            environmentId,
            input: { threadId: request.threadId },
          } as const;
          registry.refresh(previewEnvironment.list(listTarget));
          const result = await listPreviews(listTarget);
          if (result._tag === "Failure") {
            return raiseAtomCommandFailure(result);
          }
          reconcilePreviewServerSessions(threadRef, result.value.sessions);
          state = readThreadPreviewState(threadRef);
        }
        tabId = request.tabId ?? state.snapshot?.tabId ?? null;
        const unavailableTarget = {
          requestId: request.requestId,
          operation: request.operation,
          environmentId,
          threadId: request.threadId,
          tabId,
          bridgeAvailable: Boolean(previewBridge),
        };
        const requireReadyTab = async () => {
          const bridge = previewBridge;
          const readyTabId = tabId;
          if (!bridge || !readyTabId) {
            throw new PreviewAutomationTargetUnavailableError(unavailableTarget);
          }
          await waitForDesktopOverlay(threadRef, request.requestId, readyTabId, request.timeoutMs);
          return { bridge, tabId: readyTabId };
        };
        const context = requestContext(environmentId, request);
        switch (request.operation) {
          case "status": {
            const status = await currentStatus(threadRef, tabId);
            assertNoLocalFailure(threadRef, status.tabId ?? tabId, context);
            return status;
          }
          case "open": {
            const input = request.input as PreviewAutomationOpenInput;
            let activeTabId = resolvePreviewAutomationOpenTab(
              state,
              request.tabId,
              input.reuseExistingTab ?? true,
            );
            let activeSnapshot = activeTabId
              ? (state.sessions[activeTabId] ?? state.snapshot ?? undefined)
              : undefined;
            const reusedExistingTab = activeTabId !== null;
            tabId = activeTabId;
            // Resolve first: a server-loopback URL must never load this
            // computer's localhost for a remote environment. Shared state
            // stores the canonical URL; the native view resolves it per client.
            const canonicalUrl = input.url
              ? resolveBrowserNavigationTarget(environmentId, { kind: "url", url: input.url })
                  .resolvedUrl
              : null;
            if (!activeTabId) {
              const result = await open({
                environmentId,
                input: {
                  threadId: request.threadId,
                  ...(canonicalUrl ? { url: canonicalUrl } : {}),
                },
              });
              if (result._tag === "Failure") {
                return raiseAtomCommandFailure(result);
              }
              const snapshot = result.value;
              applyPreviewServerSnapshot(threadRef, snapshot);
              activeTabId = snapshot.tabId;
              activeSnapshot = snapshot;
              tabId = activeTabId;
            }
            // The desktop host has one native view and drives only the visible
            // tab, so a navigation-only bridge cannot honor `show: false`.
            if (
              (input.show ?? true) ||
              !supportsPreviewRuntimeCapability(previewBridge, "automation")
            ) {
              useRightPanelStore.getState().openBrowser(threadRef, activeTabId);
            }
            if (activeSnapshot && previewAutomationOpenNeedsOverlay(input, activeSnapshot)) {
              await waitForDesktopOverlay(
                threadRef,
                request.requestId,
                activeTabId,
                request.timeoutMs,
              );
              if (!reusedExistingTab && canonicalUrl !== null) {
                await waitForFirstNavigation(threadRef, activeTabId, request.timeoutMs, context);
              }
            }
            if (reusedExistingTab && canonicalUrl !== null && previewBridge) {
              await previewBridge.navigate(
                activeTabId,
                await resolveNativeUrl(threadRef, activeTabId, canonicalUrl, gatewayOpen, context),
              );
              await waitForNavigationReadiness(
                threadRef,
                context,
                activeTabId,
                "load",
                request.timeoutMs,
              );
            }
            const status = await currentStatus(threadRef, activeTabId);
            // A reused tab may have failed on this client; never report it as fine.
            assertNoLocalFailure(threadRef, activeTabId, context);
            return status;
          }
          case "navigate": {
            const ready = await requireReadyTab();
            const input = request.input as PreviewAutomationNavigateInput;
            const resolution = resolveBrowserNavigationTarget(
              environmentId,
              input.target ?? {
                kind: "url",
                url: input.url!,
              },
            );
            await ready.bridge.navigate(
              ready.tabId,
              await resolveNativeUrl(
                threadRef,
                ready.tabId,
                resolution.resolvedUrl,
                gatewayOpen,
                context,
              ),
            );
            await waitForNavigationReadiness(
              threadRef,
              context,
              ready.tabId,
              input.readiness ?? "load",
              input.timeoutMs ?? request.timeoutMs,
            );
            return await currentStatus(threadRef, ready.tabId);
          }
          case "resize": {
            const ready = await requireReadyTab();
            const input = request.input as PreviewAutomationResizeInput;
            const setting = resolvePreviewViewport(input);
            const result = await resize({
              environmentId,
              input: {
                threadId: request.threadId,
                tabId: ready.tabId,
                viewport: setting,
              },
            });
            if (result._tag === "Failure") {
              return raiseAtomCommandFailure(result);
            }
            updatePreviewServerSnapshot(threadRef, result.value);
            const viewport = await waitForRenderedViewport(
              ready.tabId,
              setting,
              input.timeoutMs ?? request.timeoutMs,
              {
                requestId: request.requestId,
                environmentId,
                threadId: request.threadId,
              },
            );
            return {
              tabId: ready.tabId,
              setting,
              viewport,
            } satisfies PreviewAutomationResizeResult;
          }
          case "snapshot": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.snapshot(ready.tabId);
          }
          case "click": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.click(
              ready.tabId,
              request.input as Parameters<typeof ready.bridge.automation.click>[1],
            );
          }
          case "type": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.type(
              ready.tabId,
              request.input as Parameters<typeof ready.bridge.automation.type>[1],
            );
          }
          case "press": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.press(
              ready.tabId,
              request.input as Parameters<typeof ready.bridge.automation.press>[1],
            );
          }
          case "scroll": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.scroll(
              ready.tabId,
              request.input as Parameters<typeof ready.bridge.automation.scroll>[1],
            );
          }
          case "evaluate": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.evaluate(
              ready.tabId,
              request.input as Parameters<typeof ready.bridge.automation.evaluate>[1],
            );
          }
          case "waitFor": {
            const ready = await requireReadyTab();
            return await ready.bridge.automation.waitFor(
              ready.tabId,
              request.input as Parameters<typeof ready.bridge.automation.waitFor>[1],
            );
          }
          case "recordingStart": {
            const ready = await requireReadyTab();
            const startedAt = await startBrowserRecording(ready.tabId);
            return {
              tabId: ready.tabId,
              recording: true,
              startedAt,
            };
          }
          case "recordingStop": {
            const recordingTabId = readActiveBrowserRecordingTabId();
            const stopTabId = resolveBrowserRecordingStopTarget(
              recordingTabId,
              request.tabIdExplicit ? request.tabId : undefined,
            );
            const artifact = stopTabId ? await stopBrowserRecording(stopTabId) : null;
            if (!artifact) {
              return raisePreviewAutomationHostError(
                new PreviewAutomationRecordingNotActiveError({
                  requestId: request.requestId,
                  environmentId,
                  threadId: request.threadId,
                  tabId,
                }),
              );
            }
            return artifact;
          }
        }
      } catch (cause) {
        throw PreviewAutomationOperationError.fromCause({
          requestId: request.requestId,
          operation: request.operation,
          environmentId,
          threadId: request.threadId,
          tabId,
          cause,
        });
      }
    },
    [environmentId, gatewayOpen, listPreviews, open, registry, resize],
  );
  const [requestHandlerAtom] = useState(() => Atom.make({ handle: handleRequest }));
  const setRequestHandler = useAtomSet(requestHandlerAtom);
  useEffect(() => {
    setRequestHandler({ handle: handleRequest });
  }, [handleRequest, setRequestHandler]);

  const automationRequestConsumerAtom = useMemo(
    () =>
      createPreviewAutomationRequestConsumerAtom({
        requestsAtom: automationRequestsAtom,
        clientId: automationClientId,
        connectionAtom: automationConnectionAtom,
        environmentId,
        requestHandlerAtom,
        respond: (response) =>
          respondToAutomation({
            environmentId,
            input: response,
          }),
        label: `preview:automation-host:${environmentId}:${automationClientId}`,
      }),
    [
      automationClientId,
      automationConnectionAtom,
      automationRequestsAtom,
      requestHandlerAtom,
      respondToAutomation,
      environmentId,
    ],
  );
  useAtomValue(automationRequestConsumerAtom);

  useEffect(() => {
    const report = () => {
      if (!automationConnectionId) return;
      void focusAutomationHost({
        environmentId,
        input: {
          clientId: automationClientId,
          environmentId,
          connectionId: automationConnectionId,
          focused: document.hasFocus(),
        },
      });
    };
    report();
    window.addEventListener("focus", report);
    window.addEventListener("blur", report);
    return () => {
      window.removeEventListener("focus", report);
      window.removeEventListener("blur", report);
    };
  }, [automationClientId, automationConnectionId, environmentId, focusAutomationHost]);

  return null;
}
