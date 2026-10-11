import type { EnvironmentId, ScopedThreadRef } from "@bibcode/contracts";
import { isAtomCommandInterrupted, runAtomCommand } from "@bibcode/client-runtime/state/runtime";

import { isBrowserMode, isThisPageHost } from "~/components/preview/previewBridge";
import { getClientSettings } from "~/hooks/useSettings";
import { readLocalApi } from "~/localApi";
import { canPreviewAnyUrlInRuntime, isPreviewSupportedInRuntime } from "~/previewStateStore";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import { previewEnvironment } from "~/state/preview";

import { openPendingTab } from "./browserTab";
import { enqueueOpenPrompt } from "./openPromptQueue";
import { resolvePreviewTarget } from "./browserTargetResolver";
import { type OpenPreviewMutation, openUrlInPreview } from "./openFileInPreview";
import {
  showLinkOpenFailedNotice,
  showPreviewUnreachableMessage,
  showPreviewUnreachableNotice,
} from "./linkNotices";
import { type GatewayOpenMutation, resolveForNavigation } from "./previewGateway";

export type LinkDestination = "app" | "system";
export type OpenLinkOutcome = "app" | "system" | "unreachable" | "unavailable";

export function chooseLinkDestination(input: {
  readonly setting: LinkDestination;
  readonly invert: boolean;
  readonly canUseApp: boolean;
}): LinkDestination {
  if (!input.canUseApp) return "system";
  if (!input.invert) return input.setting;
  return input.setting === "app" ? "system" : "app";
}

export function openLink(input: {
  readonly url: string;
  readonly threadRef: ScopedThreadRef | null;
  /** Resolves server-loopback URLs when there is no thread (e.g. a thread-less terminal). */
  readonly environmentId?: EnvironmentId | null;
  readonly invert: boolean;
  readonly openPreview: OpenPreviewMutation<unknown>;
  /** Called for internal-browser failures only; a system-browser failure shows its own notice. */
  readonly onError?: (cause: unknown) => void;
  /**
   * Browser mode only: the new tab was blocked. Without it, a direct address
   * opens through the local API and a blocked gateway address queues the open prompt.
   */
  readonly onPopupBlocked?: (url: string) => void;
  /** A gateway address failed to open after this returned, in a way a later try may fix. */
  readonly onUnopened?: () => void;
}): OpenLinkOutcome {
  let url = input.url;
  let viaGateway = false;
  let gatewayHost: string | null = null;
  const environmentId = input.threadRef?.environmentId ?? input.environmentId ?? null;
  if (environmentId !== null) {
    const resolution = resolvePreviewTarget(environmentId, url);
    if (resolution.kind === "unreachable") {
      showPreviewUnreachableNotice(resolution);
      return "unreachable";
    }
    // A gateway URL stays canonical: the internal browser resolves it per client.
    url = resolution.url;
    // A same-host page needs the gateway only inside the frame; a new tab or a
    // thread-less open loads it directly.
    viaGateway = resolution.kind === "gateway" && !(resolution.via === "host" && resolution.direct);
    gatewayHost =
      resolution.kind === "gateway" && resolution.via === "host" ? resolution.host : null;
  }
  const destination = chooseLinkDestination({
    setting: getClientSettings().browserLinkTarget,
    invert: input.invert,
    // Browser mode frames only gateway previews on this page's host; other
    // addresses open in a new tab.
    canUseApp:
      input.threadRef !== null &&
      (canPreviewAnyUrlInRuntime() ||
        (gatewayHost !== null && isPreviewSupportedInRuntime() && isThisPageHost(gatewayHost))),
  });
  if (destination === "system" || input.threadRef === null) {
    if (viaGateway) {
      if (input.threadRef === null) {
        // The gateway admits targets per thread; there is none to admit this one.
        showPreviewUnreachableMessage(
          "Open this address from a thread's chat or terminal to reach it from here.",
          url,
        );
        return "unreachable";
      }
      return openGatewayInSystemBrowser(url, input.threadRef, input);
    }
    if (input.onPopupBlocked && isBrowserMode()) {
      // The caller handles a blocked tab, which a "noopener" window.open cannot report.
      const tab = openPendingTab();
      if (tab) tab.navigate(url);
      else input.onPopupBlocked(url);
      return "system";
    }
    const api = readLocalApi();
    if (!api) return "unavailable";
    // Called synchronously so browser-mode window.open keeps the click's activation.
    api.shell.openExternal(url).catch(() => showLinkOpenFailedNotice(url));
    return "system";
  }
  openUrlInPreview({ threadRef: input.threadRef, url, openPreview: input.openPreview })
    .then((result) => {
      if (result._tag === "Failure" && !isAtomCommandInterrupted(result))
        input.onError?.(result.cause);
    })
    .catch((cause: unknown) => input.onError?.(cause));
  return "app";
}

const gatewayOpen: GatewayOpenMutation = (input) =>
  runAtomCommand(appAtomRegistry, previewEnvironment.gatewayOpen, input, { reportFailure: false });

/**
 * Hands a server-loopback address to the system browser through a fresh
 * gateway capability. Nothing is cached: every open resolves again, so a
 * closed gateway target is restarted by opening the link again.
 */
function openGatewayInSystemBrowser(
  canonicalUrl: string,
  threadRef: ScopedThreadRef,
  callbacks: {
    readonly onPopupBlocked?: ((url: string) => void) | undefined;
    readonly onUnopened?: (() => void) | undefined;
  },
): OpenLinkOutcome {
  const unopened = (notify: () => void, retryable: boolean) => {
    notify();
    if (retryable) callbacks.onUnopened?.();
  };
  const resolve = () =>
    resolveForNavigation({
      environmentId: threadRef.environmentId,
      threadId: threadRef.threadId,
      canonicalUrl,
      gatewayOpen,
    });
  if (!isBrowserMode()) {
    // The desktop window refuses window.open; the bridge opens it without one.
    const api = readLocalApi();
    if (!api) return "unavailable";
    void resolve()
      .then((target) => {
        if (target.kind === "unreachable") {
          unopened(
            () => showPreviewUnreachableMessage(target.message, canonicalUrl),
            target.retryable === true,
          );
          return;
        }
        return api.shell
          .openExternal(target.url)
          .catch(() => unopened(() => showLinkOpenFailedNotice(canonicalUrl), true));
      })
      .catch(() => unopened(() => showLinkOpenFailedNotice(canonicalUrl), true));
    return "system";
  }
  // Opened before any await: the click's user activation does not survive one.
  const tab = openPendingTab();
  if (!tab) {
    if (callbacks.onPopupBlocked) callbacks.onPopupBlocked(canonicalUrl);
    else enqueueOpenPrompt({ source: "link", blocked: true, url: canonicalUrl, threadRef });
    return "system";
  }
  void resolve()
    .then((target) => {
      if (target.kind === "ok") {
        tab.navigate(target.url);
        return;
      }
      tab.close();
      unopened(
        () => showPreviewUnreachableMessage(target.message, canonicalUrl),
        target.retryable === true,
      );
    })
    .catch(() => {
      tab.close();
      unopened(() => showLinkOpenFailedNotice(canonicalUrl), true);
    });
  return "system";
}
