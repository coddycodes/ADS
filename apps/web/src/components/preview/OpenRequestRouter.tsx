"use client";

import { RegistryContext } from "@effect/atom-react";
import { scopedThreadKey, scopeThreadRef } from "@bibcode/client-runtime/environment";
import type { EnvironmentId, ScopedThreadRef, ThreadId } from "@bibcode/contracts";
import { AsyncResult } from "effect/reactivity";
import { useContext, useEffect } from "react";

import { openLink } from "~/browser/openLink";
import { enqueueOpenPrompt } from "~/browser/openPromptQueue";
import { useCenterPanelStore } from "~/centerPanelStore";
import { getClientSettings } from "~/hooks/useSettings";
import type { AppRouter } from "~/router";
import { readThreadShell } from "~/state/entities";
import { useEnvironments } from "~/state/environments";
import { previewEnvironment } from "~/state/preview";
import { useAtomCommand } from "~/state/use-atom-command";
import { resolveThreadRouteRef } from "~/threadRoutes";

import { isBrowserMode } from "./previewBridge";

type ScreenRouter = Pick<AppRouter, "state">;

/** How long a client that doesn't show the thread, or is hidden, waits before claiming. */
const CLAIM_DELAY_MS = 2_000;
/** Request ids remembered against replays; requests expire after 60 s anyway. */
const HANDLED_LIMIT = 256;

/** Whether a thread is the routed one, a chat panel shown as an active center tab, or neither. */
function shownAs(host: ScopedThreadRef, threadId: string): "route" | "panel" | null {
  if (host.threadId === threadId) return "route";
  const layout = useCenterPanelStore.getState().byThreadKey[scopedThreadKey(host)];
  return layout?.groups.some((group) => group.activeSurfaceId === `chat:${threadId}`)
    ? "panel"
    : null;
}

/**
 * Opens what a thread's commands ask to open (`$BROWSER`, `bibcode-open-url`).
 * Every client sees the request and one claims it: a visible client showing
 * that thread at once; otherwise, after a short delay, a visible client asks
 * the user in a prompt naming the thread (or a hidden one showing the thread).
 */
export function OpenRequestRouter(props: { readonly router: ScreenRouter }) {
  const { environments } = useEnvironments();
  return (
    <>
      {environments.map((environment) => (
        <EnvironmentOpenRequestRouter
          key={environment.environmentId}
          environmentId={environment.environmentId}
          router={props.router}
        />
      ))}
    </>
  );
}

function EnvironmentOpenRequestRouter(props: {
  readonly environmentId: EnvironmentId;
  readonly router: ScreenRouter;
}) {
  const { environmentId, router } = props;
  const registry = useContext(RegistryContext);
  const claimOpenRequest = useAtomCommand(previewEnvironment.claimOpenRequest, {
    reportFailure: false,
  });
  const openPreview = useAtomCommand(previewEnvironment.open, { reportFailure: true });

  useEffect(() => {
    // Insertion-ordered, so the first entry is the oldest.
    const handled = new Set<string>();
    const timers = new Set<number>();
    let subscribed = false;
    let disposed = false;
    // `immediate` starts the stream; the value it replays was already delivered.
    const unsubscribe = registry.subscribe(
      previewEnvironment.events({ environmentId, input: {} }),
      (result) => {
        if (!subscribed || !AsyncResult.isSuccess(result)) return;
        const event = result.value;
        if (event.type !== "openRequested" || handled.has(event.requestId)) return;
        handled.add(event.requestId);
        if (handled.size > HANDLED_LIMIT) handled.delete(handled.values().next().value!);
        const threadRef = scopeThreadRef(environmentId, event.threadId as ThreadId);
        const shown = () => {
          const host = resolveThreadRouteRef(router.state.matches.at(-1)?.params ?? {});
          return host?.environmentId === environmentId ? shownAs(host, event.threadId) : null;
        };
        const claim = () => {
          // Checked when claiming: things changed during a delay.
          const visible = document.visibilityState !== "hidden";
          const onScreen = shown() !== null;
          // A hidden client claims only a thread it shows; nobody would see its prompt.
          if (!visible && !onScreen) return;
          void claimOpenRequest({ environmentId, input: { requestId: event.requestId } }).then(
            (result) => {
              if (disposed) return;
              if (result._tag !== "Success") {
                // The URL can carry a capability-bearing path; log only the request.
                console.warn("Couldn't claim the preview open request", event.requestId);
                return;
              }
              if (!result.value.claimed) return;
              // Checked again: the user may have moved on during the claim.
              const where = shown();
              if (where === null) {
                // This client doesn't show the thread: ask, naming it, and show it on Open.
                const title = readThreadShell(threadRef)?.title;
                enqueueOpenPrompt({
                  id: event.requestId,
                  source: "command",
                  url: event.url,
                  threadRef,
                  threadTitle: title ? `“${title}”` : "another thread",
                });
                return;
              }
              if (isBrowserMode()) {
                // No click backs this open, so a browser would block the new tab.
                enqueueOpenPrompt({
                  id: event.requestId,
                  source: "command",
                  url: event.url,
                  threadRef,
                });
                return;
              }
              // Only the routed thread has a browser panel here; a chat panel uses the
              // system browser.
              const invert = where !== "route" && getClientSettings().browserLinkTarget === "app";
              openLink({ url: event.url, threadRef, invert, openPreview });
            },
          );
        };
        if (document.visibilityState !== "hidden" && shown() !== null) {
          claim();
          return;
        }
        // A visible client showing the thread claims first; after the delay a
        // visible one asks about a thread off screen. Requests expire after 60 s.
        const timer = window.setTimeout(() => {
          timers.delete(timer);
          claim();
        }, CLAIM_DELAY_MS);
        timers.add(timer);
      },
      { immediate: true },
    );
    subscribed = true;
    return () => {
      disposed = true;
      unsubscribe();
      for (const timer of timers) window.clearTimeout(timer);
    };
  }, [claimOpenRequest, environmentId, openPreview, registry, router]);

  return null;
}
