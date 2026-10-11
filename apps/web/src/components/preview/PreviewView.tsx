"use client";

import { scopedThreadKey } from "@bibcode/client-runtime/environment";
import { squashAtomCommandFailure } from "@bibcode/client-runtime/state/runtime";
import {
  FILL_PREVIEW_VIEWPORT,
  type PreviewViewportSetting,
  type ScopedThreadRef,
} from "@bibcode/contracts";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";

import { useComposerDraftStore } from "~/composerDraftStore";
import { previewAnnotationScreenshotFile } from "~/lib/previewAnnotation";
import { ensureLocalApi } from "~/localApi";
import { supportsPreviewRuntimeCapability } from "~/previewRuntimeCapabilities";
import {
  rememberPreviewUrl,
  updatePreviewServerSnapshot,
  useThreadPreviewState,
} from "~/previewStateStore";
import { resolvePreviewTarget } from "~/browser/browserTargetResolver";
import { showPreviewUnreachableMessage, showPreviewUnreachableNotice } from "~/browser/linkNotices";
import { navigateDesktopTab } from "~/browser/desktopTabLifetime";
import { previewPartitionFor } from "~/browser/previewPartition";
import { canonicalizePreviewUrl, resolveForNavigation } from "~/browser/previewGateway";
import { failPreviewTabNavigation } from "~/browser/previewTabFailure";
import { useEnvironment, useEnvironmentHttpBaseUrl } from "~/state/environments";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";

import { previewBridge } from "./previewBridge";
import { subscribePreviewAction } from "./previewActionBus";
import { openPreviewSession } from "./openPreviewSession";
import { PreviewChromeRow } from "./PreviewChromeRow";
import { formatPreviewUrl } from "./previewUrlPresentation";
import { PreviewEmptyState } from "./PreviewEmptyState";
import { PreviewMoreMenu } from "./PreviewMoreMenu";
import {
  commitBrowserViewportChange,
  subscribeBrowserViewportChange,
} from "~/browser/browserViewportActions";
import { resolveResponsiveBrowserViewportSize } from "~/browser/browserViewportLayout";
import { BrowserDeviceToolbar } from "~/browser/BrowserDeviceToolbar";
import { BrowserViewportResizeHandles } from "~/browser/BrowserViewportResizeHandles";
import { useBrowserViewportResize } from "~/browser/useBrowserViewportResize";
import { PreviewUnreachable } from "./PreviewUnreachable";
import { revealInFileExplorerLabel } from "./fileExplorerLabel";
import { shouldShowPreviewEmptyState } from "./previewEmptyStateLogic";
import { BrowserSurfaceSlot } from "~/browser/BrowserSurfaceSlot";
import { useBrowserSurfaceStore } from "~/browser/browserSurfaceStore";
import { useLoadingProgress } from "./useLoadingProgress";
import { usePreviewSession } from "./usePreviewSession";
import { ZoomIndicator } from "./ZoomIndicator";
import { AgentBrowserCursor } from "./AgentBrowserCursor";
import {
  startBrowserRecording,
  stopBrowserRecording,
  useActiveBrowserRecordingTabId,
} from "~/browser/browserRecording";
import { stackedThreadToast, toastManager } from "~/components/ui/toast";

interface Props {
  threadRef: ScopedThreadRef;
  tabId?: string | null;
  configuredUrls?: ReadonlyArray<string> | undefined;
  visible: boolean;
}

const localApi = typeof window === "undefined" ? null : ensureLocalApi();

/**
 * Single-tab preview surface: chrome row on top, one webview below, empty
 * state when no session exists for the thread.
 */
export function PreviewView({ threadRef, tabId: requestedTabId, configuredUrls, visible }: Props) {
  const [focusUrlNonce, setFocusUrlNonce] = useState<number | undefined>(undefined);
  const [pickActive, setPickActive] = useState(false);
  const [browserChromeOverlayOpen, setBrowserChromeOverlayOpen] = useState(false);
  const [aspectRatio, setAspectRatio] = useState<number | null>(null);
  const [containerSize, setContainerSize] = useState({ width: 1, height: 1 });
  const activeRecordingTabId = useActiveBrowserRecordingTabId();
  const browserContainerRef = useRef<HTMLDivElement | null>(null);
  const pickActiveRef = useRef(false);
  const isMountedRef = useRef(true);
  /** The latest URL submission; an older one still resolving must not win. */
  const submissionRef = useRef(0);
  const previewState = useThreadPreviewState(threadRef);
  const addPreviewAnnotation = useComposerDraftStore((store) => store.addPreviewAnnotation);
  const addAttachment = useComposerDraftStore((store) => store.addAttachment);
  const environment = useEnvironment(threadRef.environmentId);
  const environmentHttpBaseUrl = useEnvironmentHttpBaseUrl(threadRef.environmentId);
  const open = useAtomCommand(previewEnvironment.open);
  const navigate = useAtomCommand(previewEnvironment.navigate, "preview navigation");
  const resize = useAtomCommand(previewEnvironment.resize, "preview viewport resize");
  // Gateway refusals become a notice or the tab's failed state, with their own copy.
  const gatewayOpen = useAtomCommand(previewEnvironment.gatewayOpen, { reportFailure: false });
  const reportStatus = useAtomCommand(previewEnvironment.reportStatus, "preview status report");

  usePreviewSession(threadRef);

  useEffect(() => {
    isMountedRef.current = true;
    return () => {
      isMountedRef.current = false;
    };
  }, []);

  const tabId = requestedTabId ?? previewState.activeTabId;

  // A submission or reload still resolving belongs to the tab it started on.
  useEffect(
    () => () => {
      submissionRef.current += 1;
    },
    [tabId, threadRef.environmentId, threadRef.threadId],
  );
  const snapshot = tabId ? (previewState.sessions[tabId] ?? null) : null;
  const desktopOverlay = tabId ? (previewState.desktopByTabId[tabId] ?? null) : null;
  const navStatus = snapshot?.navStatus ?? { _tag: "Idle" as const };
  const snapshotUrl = navStatus._tag === "Idle" ? "" : navStatus.url;
  const url = desktopOverlay?.url ?? snapshotUrl;
  const loading = desktopOverlay?.loading ?? navStatus._tag === "Loading";
  const canGoBack = desktopOverlay?.canGoBack ?? snapshot?.canGoBack ?? false;
  const canGoForward = desktopOverlay?.canGoForward ?? snapshot?.canGoForward ?? false;
  const refreshDisabled = navStatus._tag === "Idle";
  // This client's own failure wins: the shared tab may be fine for others.
  const localFailure = tabId ? previewState.localFailures[tabId] : undefined;
  const failure = localFailure ?? (navStatus._tag === "LoadFailed" ? navStatus : null);
  const isUnreachable = failure !== null;
  const showEmptyState = shouldShowPreviewEmptyState(snapshot);
  const controller = desktopOverlay?.controller ?? "none";
  const loadProgress = useLoadingProgress(loading);
  const displayUrl =
    url && environment && environmentHttpBaseUrl
      ? (formatPreviewUrl({
          url,
          environmentLabel: environment.label,
          environmentHttpBaseUrl,
        }) ?? undefined)
      : undefined;
  const viewport = snapshot?.viewport ?? FILL_PREVIEW_VIEWPORT;
  const panelRect = useBrowserSurfaceStore((state) =>
    tabId ? (state.byTabId[tabId]?.rect ?? null) : null,
  );
  const pickerSupported = supportsPreviewRuntimeCapability(previewBridge, "picker");
  const recordingSupported = supportsPreviewRuntimeCapability(previewBridge, "recording");
  const imageClipboardSupported = supportsPreviewRuntimeCapability(previewBridge, "imageClipboard");
  const viewportResize = useBrowserViewportResize({
    tabId: tabId ?? "",
    viewport,
    zoomFactor: desktopOverlay?.zoomFactor ?? 1,
    containerSize,
    deviceToolbarVisible: viewport._tag !== "fill",
    aspectRatio,
  });

  useLayoutEffect(() => {
    const element = browserContainerRef.current;
    if (!element || typeof ResizeObserver === "undefined") return;
    const update = () => {
      const rect = element.getBoundingClientRect();
      setContainerSize({ width: rect.width, height: rect.height });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);

  /**
   * Resolves a canonical URL to what this client can load. Returns null when
   * it can't (after showing why) or when `isCurrent` says the request was
   * overtaken meanwhile (silently). `forTabId` ties an SSH forward to that tab.
   */
  const resolveForThisClient = useCallback(
    async (canonicalUrl: string, forTabId: string | undefined, isCurrent: () => boolean) => {
      const target = await resolveForNavigation({
        environmentId: threadRef.environmentId,
        threadId: threadRef.threadId,
        canonicalUrl,
        gatewayOpen,
        ...(forTabId === undefined ? {} : { tabId: forTabId }),
      });
      if (!isCurrent()) return null;
      if (target.kind === "ok") return target.url;
      showPreviewUnreachableMessage(target.message, canonicalUrl);
      return null;
    },
    [gatewayOpen, threadRef],
  );

  const handleSubmitUrl = useCallback(
    async (next: string) => {
      const submission = ++submissionRef.current;
      const superseded = () => submission !== submissionRef.current || !isMountedRef.current;
      try {
        const resolution = resolvePreviewTarget(threadRef.environmentId, next);
        if (resolution.kind === "unreachable") {
          showPreviewUnreachableNotice(resolution);
          return;
        }
        // Shared state carries the canonical URL; only this client's webview
        // gets the address it can actually load.
        const canonicalUrl = resolution.url;
        if (tabId && previewBridge) {
          // Resolve first so a refused gateway leaves the shared tab untouched.
          const resolvedUrl = await resolveForThisClient(canonicalUrl, tabId, () => !superseded());
          if (resolvedUrl === null) return;
          // Commit the canonical snapshot before driving the native webview.
          // Native Loading/Success events can then enrich that snapshot
          // without a fast load leaving a new tab stuck on Idle or allowing
          // the initial server response to overwrite the final page title.
          const result = await navigate({
            environmentId: threadRef.environmentId,
            input: {
              threadId: threadRef.threadId,
              tabId,
              url: canonicalUrl,
            },
          });
          if (result._tag === "Failure") throw squashAtomCommandFailure(result);
          updatePreviewServerSnapshot(threadRef, result.value);
          if (superseded()) return;
          await navigateDesktopTab(
            tabId,
            previewPartitionFor(threadRef.environmentId),
            resolvedUrl,
          );
          rememberPreviewUrl(threadRef, canonicalUrl);
        } else {
          await openPreviewSession({
            openPreview: open,
            threadRef,
            url: canonicalUrl,
          });
        }
      } catch {
        // Server-side `failed` event renders the unreachable view.
      }
    },
    [navigate, open, resolveForThisClient, tabId, threadRef],
  );

  const handleRefresh = useCallback(() => {
    if (!previewBridge || !tabId) return;
    // A gateway capability is single use, so reloading a gateway tab
    // re-bootstraps it (and re-establishes any SSH forward). A tab this
    // client failed resolves its failed URL again whatever the target: the
    // native view may still hold the previous page, or nothing at all.
    const reloadUrl = localFailure?.url ?? url;
    if (
      !reloadUrl ||
      (!localFailure && resolvePreviewTarget(threadRef.environmentId, reloadUrl).kind !== "gateway")
    ) {
      void previewBridge.refresh(tabId);
      return;
    }
    // A reload is a navigation too: a newer submission or unmount cancels it.
    const submission = ++submissionRef.current;
    const isCurrent = () => submission === submissionRef.current && isMountedRef.current;
    void resolveForNavigation({
      environmentId: threadRef.environmentId,
      threadId: threadRef.threadId,
      canonicalUrl: reloadUrl,
      gatewayOpen,
      tabId,
    })
      .then((target) => {
        if (!isCurrent()) return;
        if (target.kind === "ok") {
          return navigateDesktopTab(
            tabId,
            previewPartitionFor(threadRef.environmentId),
            target.url,
          );
        }
        // Nothing loads, so fail the tab (here only, or shared for a refusal).
        return failPreviewTabNavigation({
          threadRef,
          tabId,
          url: reloadUrl,
          failure: target,
          reportStatus,
        });
      })
      .catch(() => undefined);
  }, [gatewayOpen, localFailure, reportStatus, tabId, threadRef, url]);

  const handleZoomIn = useCallback(() => {
    if (previewBridge && tabId) void previewBridge.zoomIn(tabId);
  }, [tabId]);

  const handleZoomOut = useCallback(() => {
    if (previewBridge && tabId) void previewBridge.zoomOut(tabId);
  }, [tabId]);

  const handleResetZoom = useCallback(() => {
    if (previewBridge && tabId) void previewBridge.resetZoom(tabId);
  }, [tabId]);

  const handleViewportChange = useCallback(
    async (nextViewport: PreviewViewportSetting) => {
      if (!tabId) return;
      const result = await resize({
        environmentId: threadRef.environmentId,
        input: {
          threadId: threadRef.threadId,
          tabId,
          viewport: nextViewport,
        },
      });
      if (result._tag === "Failure") {
        const error = squashAtomCommandFailure(result);
        toastManager.add({
          type: "error",
          title: "Unable to resize browser viewport",
          description: error instanceof Error ? error.message : "An error occurred.",
        });
        throw error;
      }
      updatePreviewServerSnapshot(threadRef, result.value);
    },
    [resize, tabId, threadRef],
  );

  const handleToggleDeviceToolbar = () => {
    if (!tabId) return;
    if (viewport._tag !== "fill") {
      void commitBrowserViewportChange(tabId, FILL_PREVIEW_VIEWPORT).catch(() => undefined);
      return;
    }

    const responsiveSize = panelRect
      ? resolveResponsiveBrowserViewportSize(panelRect, desktopOverlay?.zoomFactor)
      : { width: 1024, height: 768 };
    void commitBrowserViewportChange(tabId, { _tag: "freeform", ...responsiveSize }).catch(
      () => undefined,
    );
  };

  useEffect(() => {
    if (!tabId) return;
    return subscribeBrowserViewportChange(tabId, handleViewportChange);
  }, [handleViewportChange, tabId]);

  const handleBack = useCallback(() => {
    if (previewBridge && tabId) void previewBridge.goBack(tabId);
  }, [tabId]);

  const handleForward = useCallback(() => {
    if (previewBridge && tabId) void previewBridge.goForward(tabId);
  }, [tabId]);

  const handleOpenInBrowser = useCallback(() => {
    if (!localApi || !url) return;
    // The stored URL is canonical; a remote server's loopback must not open
    // this computer's localhost.
    void resolveForThisClient(url, undefined, () => isMountedRef.current)
      .then((resolved) => (resolved === null ? undefined : localApi.shell.openExternal(resolved)))
      .catch(() => undefined);
  }, [resolveForThisClient, url]);

  const handleCapture = useCallback(
    (record: boolean) => {
      if (!previewBridge || !tabId) return;
      const bridge = previewBridge;
      const recordingThisTab = activeRecordingTabId === tabId;
      if (recordingThisTab) {
        void stopBrowserRecording(tabId).then(
          (artifact) => {
            if (!artifact) return;
            let pathCopied = false;
            let toastId: ReturnType<typeof toastManager.add>;

            const copyPath = () => {
              if (!navigator.clipboard?.writeText) {
                toastManager.update(
                  toastId,
                  stackedThreadToast({
                    type: "error",
                    title: "Unable to copy recording path",
                    description: "Clipboard API unavailable.",
                    actionProps: revealAction,
                  }),
                );
                return;
              }

              void navigator.clipboard.writeText(artifact.path).then(
                () => {
                  pathCopied = true;
                  updateRecordingToast();
                  window.setTimeout(() => {
                    pathCopied = false;
                    updateRecordingToast();
                  }, 2_000);
                },
                (error) => {
                  toastManager.update(
                    toastId,
                    stackedThreadToast({
                      type: "error",
                      title: "Unable to copy recording path",
                      description: error instanceof Error ? error.message : "An error occurred.",
                      actionProps: revealAction,
                    }),
                  );
                },
              );
            };

            const revealAction = {
              children: revealInFileExplorerLabel(navigator.platform),
              onClick: () => void bridge.revealArtifact(artifact.path),
            };
            const updateRecordingToast = () => {
              toastManager.update(
                toastId,
                stackedThreadToast({
                  type: "success",
                  title: "Recording saved",
                  actionProps: revealAction,
                  data: {
                    secondaryActionProps: {
                      children: pathCopied ? "Copied!" : "Copy path",
                      disabled: pathCopied,
                      onClick: copyPath,
                    },
                    secondaryActionVariant: "outline",
                  },
                }),
              );
            };

            toastId = toastManager.add(
              stackedThreadToast({
                type: "success",
                title: "Recording saved",
                actionProps: revealAction,
                data: {
                  secondaryActionProps: {
                    children: "Copy path",
                    onClick: copyPath,
                  },
                  secondaryActionVariant: "outline",
                },
              }),
            );
          },
          (error) => {
            toastManager.add({
              type: "error",
              title: "Unable to stop recording",
              description: error instanceof Error ? error.message : "An error occurred.",
            });
          },
        );
        return;
      }
      if (record) {
        if (activeRecordingTabId !== null) {
          toastManager.add({
            type: "warning",
            title: "Another preview is recording",
            description: "Stop the active recording before starting a new one.",
          });
          return;
        }
        void startBrowserRecording(tabId).catch((error) => {
          toastManager.add({
            type: "error",
            title: "Unable to start recording",
            description: error instanceof Error ? error.message : "An error occurred.",
          });
        });
        return;
      }
      void bridge.captureScreenshot(tabId).then(
        (artifact) => {
          const revealAction = {
            children: revealInFileExplorerLabel(navigator.platform),
            onClick: () => void bridge.revealArtifact(artifact.path),
          };
          let pathCopied = false;
          let imageCopied = false;
          let toastId: ReturnType<typeof toastManager.add>;

          const updateScreenshotToast = (
            type: "success" | "error" = "success",
            title = "Screenshot saved",
            description?: string,
          ) => {
            toastManager.update(
              toastId,
              stackedThreadToast({
                type,
                title,
                description,
                ...(imageClipboardSupported
                  ? {
                      actionProps: {
                        children: imageCopied ? "Copied!" : "Copy image",
                        disabled: imageCopied,
                        onClick: copyImage,
                      },
                    }
                  : {}),
                data: {
                  additionalActions: [
                    {
                      id: "copy-path",
                      props: {
                        children: pathCopied ? "Copied!" : "Copy path",
                        disabled: pathCopied,
                        onClick: copyPath,
                      },
                    },
                  ],
                  secondaryActionProps: {
                    ...revealAction,
                  },
                  secondaryActionVariant: "outline",
                },
              }),
            );
          };

          const copyPath = () => {
            if (!navigator.clipboard?.writeText) {
              updateScreenshotToast(
                "error",
                "Unable to copy screenshot path",
                "Clipboard API unavailable.",
              );
              return;
            }

            void navigator.clipboard.writeText(artifact.path).then(
              () => {
                pathCopied = true;
                updateScreenshotToast();
                window.setTimeout(() => {
                  pathCopied = false;
                  updateScreenshotToast();
                }, 2_000);
              },
              (error) => {
                updateScreenshotToast(
                  "error",
                  "Unable to copy screenshot path",
                  error instanceof Error ? error.message : "An error occurred.",
                );
              },
            );
          };

          const copyImage = () => {
            void bridge.copyArtifactToClipboard(artifact.path).then(
              () => {
                imageCopied = true;
                updateScreenshotToast();
                window.setTimeout(() => {
                  imageCopied = false;
                  updateScreenshotToast();
                }, 2_000);
              },
              (error) => {
                updateScreenshotToast(
                  "error",
                  "Unable to copy screenshot",
                  error instanceof Error ? error.message : "An error occurred.",
                );
              },
            );
          };

          toastId = toastManager.add(
            stackedThreadToast({
              type: "success",
              title: "Screenshot saved",
              ...(imageClipboardSupported
                ? {
                    actionProps: {
                      children: "Copy image",
                      onClick: copyImage,
                    },
                  }
                : {}),
              data: {
                additionalActions: [
                  {
                    id: "copy-path",
                    props: {
                      children: "Copy path",
                      onClick: copyPath,
                    },
                  },
                ],
                secondaryActionProps: {
                  ...revealAction,
                },
                secondaryActionVariant: "outline",
              },
            }),
          );
        },
        (error) => {
          toastManager.add({
            type: "error",
            title: "Unable to capture screenshot",
            description: error instanceof Error ? error.message : "An error occurred.",
          });
        },
      );
    },
    [activeRecordingTabId, imageClipboardSupported, tabId],
  );

  const handlePickElement = useCallback(() => {
    if (!previewBridge || !tabId) return;
    if (pickActiveRef.current) {
      void previewBridge.cancelPickElement(tabId).catch(() => undefined);
      return;
    }
    // Snapshot whatever the user was focused on (typically the chat
    // composer textarea or the chrome-row pick button) BEFORE main steals
    // focus into the guest webContents. We restore it when the pick
    // resolves so the user's typing context isn't lost — otherwise after
    // every pick they'd have to click back into the textarea.
    const previouslyFocused =
      typeof document !== "undefined" ? (document.activeElement as HTMLElement | null) : null;
    pickActiveRef.current = true;
    setPickActive(true);
    void (async () => {
      try {
        const annotation = await previewBridge.pickElement(tabId);
        if (!annotation) return;
        // The agent sees the canonical page URL, never this client's gateway origin.
        addPreviewAnnotation(threadRef, {
          ...annotation,
          pageUrl: canonicalizePreviewUrl(annotation.pageUrl),
        });
        const screenshotFile = await previewAnnotationScreenshotFile(annotation);
        if (screenshotFile && annotation.screenshot) {
          addAttachment(threadRef, {
            type: "image",
            id: annotation.id,
            name: screenshotFile.name,
            mimeType: screenshotFile.type,
            sizeBytes: screenshotFile.size,
            previewUrl: annotation.screenshot.dataUrl,
            file: screenshotFile,
          });
        }
      } catch {
        // Picker failed (e.g. webview navigated). Treat as silent cancel.
      } finally {
        pickActiveRef.current = false;
        // Avoid `setState on unmounted component` if the panel/thread closed
        // while the pick was in flight.
        if (isMountedRef.current) setPickActive(false);
        // Best-effort: restore focus to whatever the user had before the
        // pick stole it into the guest webContents. Skip if the previously-
        // focused element was unmounted or is no longer focusable.
        if (
          previouslyFocused &&
          previouslyFocused.isConnected &&
          typeof previouslyFocused.focus === "function"
        ) {
          try {
            previouslyFocused.focus({ preventScroll: true });
          } catch {
            // Some elements throw on .focus() (detached iframes, etc.).
          }
        }
      }
    })();
  }, [addAttachment, addPreviewAnnotation, tabId, threadRef]);

  // If the active tab changes mid-pick (close, thread switch, hot restart),
  // tell main to tear down the in-flight session AND reset our local toggle
  // state so the button doesn't get stuck pressed against a stale tab id.
  useEffect(() => {
    return () => {
      if (!pickActiveRef.current) return;
      pickActiveRef.current = false;
      if (previewBridge && tabId) {
        void previewBridge.cancelPickElement(tabId).catch(() => undefined);
      }
      if (isMountedRef.current) setPickActive(false);
    };
  }, [tabId]);

  // Subscribe only while visible; `toggle-panel` is owned by ChatView's
  // URL-aware handler regardless of whether the panel is currently mounted.
  useEffect(() => {
    if (!visible) return;
    return subscribePreviewAction((action) => {
      switch (action) {
        case "refresh":
          handleRefresh();
          return;
        case "focus-url":
          setFocusUrlNonce((value) => (value ?? 0) + 1);
          return;
        case "zoom-in":
          handleZoomIn();
          return;
        case "zoom-out":
          handleZoomOut();
          return;
        case "reset-zoom":
          handleResetZoom();
          return;
        case "toggle-panel":
          return;
      }
    });
  }, [handleRefresh, handleResetZoom, handleZoomIn, handleZoomOut, visible]);

  return (
    <div
      className="flex min-h-0 flex-1 flex-col bg-background"
      data-thread-key={scopedThreadKey(threadRef)}
    >
      <PreviewChromeRow
        url={url}
        displayUrl={displayUrl}
        loading={loading}
        loadProgress={loadProgress}
        canGoBack={canGoBack}
        canGoForward={canGoForward}
        refreshDisabled={refreshDisabled}
        focusUrlNonce={focusUrlNonce}
        onBack={handleBack}
        onForward={handleForward}
        onRefresh={handleRefresh}
        onSubmit={(next) => void handleSubmitUrl(next)}
        onOpenInBrowser={tabId ? handleOpenInBrowser : undefined}
        onCapture={previewBridge && tabId ? handleCapture : undefined}
        captureDisabled={showEmptyState || isUnreachable}
        recording={tabId !== null && activeRecordingTabId === tabId}
        recordingSupported={recordingSupported}
        onPickElement={previewBridge && tabId && pickerSupported ? handlePickElement : undefined}
        pickActive={pickActive}
        // Disable when there's no tab (nothing to pick on) OR the page
        // failed to load (a React overlay covers the webview, so the
        // user wouldn't be able to actually click anything underneath).
        pickDisabled={!tabId || isUnreachable}
        pickDisabledReason={
          isUnreachable ? "Page didn't load — pick unavailable until the page renders" : undefined
        }
        trailingActions={
          previewBridge ? (
            <PreviewMoreMenu
              tabId={tabId}
              zoomFactor={desktopOverlay?.zoomFactor ?? 1}
              deviceToolbarVisible={viewport._tag !== "fill"}
              onToggleDeviceToolbar={handleToggleDeviceToolbar}
              onOpenChange={setBrowserChromeOverlayOpen}
            />
          ) : null
        }
      />

      <div ref={browserContainerRef} className="relative min-h-0 flex-1 overflow-hidden">
        {tabId && snapshot && !showEmptyState ? (
          <>
            {viewportResize.effectiveViewport._tag !== "fill" ? (
              <div
                className="absolute top-0"
                style={{
                  left: viewportResize.layout.viewportX,
                  width: viewportResize.layout.viewportWidth,
                }}
              >
                <BrowserDeviceToolbar
                  setting={viewportResize.effectiveViewport}
                  width={viewportResize.layout.viewportWidth}
                  aspectRatio={aspectRatio}
                  onAspectRatioChange={setAspectRatio}
                  onChange={viewportResize.commitViewportChange}
                  onSelectOpenChange={setBrowserChromeOverlayOpen}
                />
              </div>
            ) : null}
            <div
              className="absolute"
              style={{
                left: viewportResize.layout.viewportX,
                top: viewportResize.layout.viewportY,
                width: viewportResize.layout.viewportWidth,
                height: viewportResize.layout.viewportHeight,
              }}
            >
              <BrowserSurfaceSlot
                key={tabId}
                tabId={tabId}
                visible={visible && !isUnreachable && !browserChromeOverlayOpen}
                className="absolute inset-0 h-full w-full"
              />
            </div>
            {viewportResize.effectiveViewport._tag !== "fill" ? (
              <BrowserViewportResizeHandles
                layout={viewportResize.layout}
                activeDirection={viewportResize.activeDrag?.direction ?? null}
                onPointerDown={viewportResize.handleResizePointerDown}
                onKeyDown={viewportResize.handleResizeKeyDown}
              />
            ) : null}
          </>
        ) : null}
        {showEmptyState ? (
          <PreviewEmptyState
            environmentId={threadRef.environmentId}
            configuredUrls={configuredUrls}
            recentlySeenUrls={previewState.recentlySeenUrls}
            onOpenUrl={(next) => void handleSubmitUrl(next)}
          />
        ) : null}
        {snapshot && desktopOverlay ? (
          <ZoomIndicator zoomFactor={desktopOverlay.zoomFactor} />
        ) : null}
        {tabId && desktopOverlay && !showEmptyState && !isUnreachable ? (
          <AgentBrowserCursor
            tabId={tabId}
            zoomFactor={desktopOverlay.zoomFactor}
            controller={controller}
          />
        ) : null}
        {controller !== "none" ? (
          <div className="pointer-events-none absolute left-3 top-3 z-40 rounded-full border border-border/70 bg-background/90 px-2.5 py-1 text-xs font-medium shadow-sm backdrop-blur">
            {controller === "agent" ? "Agent controlling browser" : "Human control"}
          </div>
        ) : null}
        {failure !== null ? (
          <div className="absolute inset-0 z-10 bg-background">
            <PreviewUnreachable
              url={failure.url}
              code={failure.code}
              description={failure.description}
              onReload={handleRefresh}
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}
