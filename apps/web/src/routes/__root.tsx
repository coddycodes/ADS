import { type ServerLifecycleWelcomePayload } from "@coddy/contracts";
import { scopedProjectKey, scopeProjectRef } from "@coddy/client-runtime/environment";
import { squashAtomCommandFailure } from "@coddy/client-runtime/state/runtime";
import {
  Outlet,
  createRootRoute,
  type ErrorComponentProps,
  useLocation,
  useNavigate,
} from "@tanstack/react-router";
import { useEffect, useEffectEvent, useRef, useState } from "react";

import { APP_BASE_NAME, APP_DISPLAY_NAME, APP_STAGE_LABEL } from "../branding";
import { resolveServerBackedAppDisplayName } from "../branding.logic";
import { AppSidebarLayout } from "../components/AppSidebarLayout";
import { CommandPalette } from "../components/CommandPalette";
import { AppStatusBar, SlowRequestsStatusBar } from "../components/status-bar/AppStatusBar";
import { RelayClientInstallDialog } from "../components/cloud/RelayClientInstallDialog";
import { SshPasswordPromptDialog } from "../components/desktop/SshPasswordPromptDialog";
import { ProviderUpdateLaunchNotification } from "../components/ProviderUpdateLaunchNotification";
import { Button } from "../components/ui/button";
import {
  AnchoredToastProvider,
  stackedThreadToast,
  ToastProvider,
  toastManager,
} from "../components/ui/toast";
import { resolveAndPersistPreferredEditor } from "../editorPreferences";
import { DesktopDeepLinkRouter } from "../desktopDeepLink";
import { ManagedEndpointRelink } from "../cloud/managedEndpointRelink";
import { useClientSettings } from "../hooks/useSettings";
import {
  deriveLogicalProjectKeyFromSettings,
  derivePhysicalProjectKeyFromPath,
  selectProjectGroupingSettings,
} from "../logicalProject";
import { useUiStateStore } from "../uiStateStore";
import { syncBrowserChromeTheme } from "../hooks/useTheme";
import { startClientCollector } from "../profiler/startClientCollector";
import { startPreviewTunnelBroker } from "../browser/previewTunnelBroker";
import { appAtomRegistry } from "../rpc/atomRegistry";
import {
  abandonServerAuthBootstrap,
  resolveInitialServerAuthGateState,
} from "../environments/primary";
import { hasSavedEnvironments } from "../connection/savedEnvironments";
import {
  hasHostedPairingRequest,
  isHostedPairingApp,
  isHostedStaticApp,
  isServedByThisBackend,
  isServedSessionStalled,
  setServedSessionStalled,
  setServedPrimarySuppressed,
} from "../hostedPairing";
import { shellEnvironment } from "../state/shell";
import { useAtomValue } from "@effect/atom-react";
import { useAtomCommand } from "../state/use-atom-command";
import { useEnvironments, usePrimaryEnvironment } from "../state/environments";
import {
  primaryServerConfigAtom,
  primaryServerConfigEventAtom,
  primaryServerWelcomeAtom,
} from "../state/server";
import {
  readActiveEnvironmentId,
  readProject,
  reconcileEnvironmentSelection,
} from "../state/entities";
import { readCurrentEnvironmentPresentationPolicy } from "../connection/currentEnvironmentPresentation";
import {
  createKeybindingsUpdateToastController,
  type KeybindingsUpdateToastController,
} from "../components/KeybindingsUpdateToast.logic";

const SERVED_SESSION_CHECK_DEADLINE_MS = 10_000;

class ServedSessionCheckStalled extends Error {
  constructor() {
    super("This server did not answer its session check in time.");
  }
}

function withDeadline<A>(promise: Promise<A>, deadlineMs: number): Promise<A> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new ServedSessionCheckStalled()), deadlineMs);
    promise.then(
      (value) => {
        clearTimeout(timer);
        resolve(value);
      },
      (error: unknown) => {
        clearTimeout(timer);
        reject(error);
      },
    );
  });
}

export const Route = createRootRoute({
  beforeLoad: async ({ location }) => {
    const url = new URL(window.location.href);
    const hostedStatic = isHostedStaticApp(url);
    // The pairing surface retains the token in memory and removes it from the
    // URL. Keep that route active when the history replacement reloads the gate,
    // wherever this build's host-based pairing links land, a served page included.
    const hostedPairingHost =
      isHostedPairingApp(url) && url.searchParams.has("host") && !url.searchParams.has("code");
    if (location.pathname === "/pair" && (hasHostedPairingRequest(url) || hostedPairingHost)) {
      return {
        authGateState: {
          status: "hosted-pairing",
        } as const,
      };
    }

    if (hostedStatic) {
      return {
        authGateState: {
          status: "hosted-static",
        } as const,
      };
    }

    // A page this server served, not paired with it (or unable to reach it) but paired with other
    // servers through a pairing link, opens as their remote client instead of bouncing back to
    // this server's pairing. It then does not offer this server, which it cannot connect to,
    // until it is paired. Its own pairing route keeps this server's session check. A catalog that
    // did not load may hold a server just paired, so only one known to be empty sends the page to
    // pair this one.
    const servedRemoteClient = async (): Promise<boolean> => {
      if (location.pathname === "/pair" || !isServedByThisBackend(url)) return false;
      setServedPrimarySuppressed(true);
      return (await hasSavedEnvironments()) !== false;
    };
    const remoteClientState = {
      authGateState: {
        status: "hosted-static",
      } as const,
    };
    if (isServedByThisBackend(url)) {
      // Pairing this server checks its session again; the stalled check is not waited out on
      // every other navigation.
      if (location.pathname === "/pair") setServedSessionStalled(false);
      else if (isServedSessionStalled()) return remoteClientState;
    }
    let authGateState: Awaited<ReturnType<typeof resolveInitialServerAuthGateState>>;
    try {
      // On a served page, a session check that stalls must not keep the page from the other
      // servers it is paired with.
      authGateState = isServedByThisBackend(url)
        ? await withDeadline(resolveInitialServerAuthGateState(), SERVED_SESSION_CHECK_DEADLINE_MS)
        : await resolveInitialServerAuthGateState();
    } catch (error) {
      // A stalled check is abandoned whatever happens next, so the next attempt, such as
      // pairing this server or a retry once it recovers, asks the server again.
      if (error instanceof ServedSessionCheckStalled) abandonServerAuthBootstrap();
      if (await servedRemoteClient()) {
        if (error instanceof ServedSessionCheckStalled) setServedSessionStalled(true);
        return remoteClientState;
      }
      throw error;
    }
    if (authGateState.status !== "authenticated" && (await servedRemoteClient())) {
      return remoteClientState;
    }
    if (authGateState.status === "authenticated" || location.pathname !== "/pair") {
      setServedPrimarySuppressed(false);
    }
    return {
      authGateState,
    };
  },
  component: RootRouteView,
  errorComponent: RootRouteErrorView,
  head: () => ({
    meta: [{ name: "title", content: APP_DISPLAY_NAME }],
  }),
});

function RootRouteView() {
  const presentation = readCurrentEnvironmentPresentationPolicy();
  const pathname = useLocation({ select: (location) => location.pathname });
  const { authGateState } = Route.useRouteContext();
  const primaryEnvironmentAuthenticated = authGateState.status === "authenticated";

  if (pathname === "/pair") {
    return (
      <>
        <DocumentTitleSync />
        <BrowserChromeThemeSync key={pathname} />
        <Outlet />
      </>
    );
  }

  if (authGateState.status !== "authenticated" && authGateState.status !== "hosted-static") {
    return (
      <>
        <DocumentTitleSync />
        <BrowserChromeThemeSync key={pathname} />
        <Outlet />
      </>
    );
  }

  const appShell = (
    <CommandPalette>
      <AppSidebarLayout>
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <div className="min-h-0 min-w-0 flex-1">
            <Outlet />
          </div>
          {primaryEnvironmentAuthenticated ? <AppStatusBar /> : <SlowRequestsStatusBar />}
        </div>
      </AppSidebarLayout>
    </CommandPalette>
  );

  return (
    <ToastProvider>
      <AnchoredToastProvider>
        <DocumentTitleSync />
        <BrowserChromeThemeSync key={pathname} />
        {presentation.showRemoteDeviceControls ? <RelayClientInstallDialog /> : null}
        {presentation.showRemoteDeviceControls ? <SshPasswordPromptDialog /> : null}
        <DesktopDeepLinkRouter />
        {/* Only a page with no primary of its own falls back to a saved environment. On the
            desktop and an authenticated served page the primary registers later than the saved
            environments, which would otherwise win the first, unselected render. */}
        {authGateState.status === "hosted-static" ? <HostedStaticEnvironmentBootstrap /> : null}
        <ProfilerClientCollector />
        {primaryEnvironmentAuthenticated ? <EventRouter /> : null}
        {/* Reads the primary's Connect link state, so only once the primary authenticated. */}
        {primaryEnvironmentAuthenticated ? <ManagedEndpointRelink /> : null}
        {primaryEnvironmentAuthenticated ? <ProviderUpdateLaunchNotification /> : null}
        {appShell}
      </AnchoredToastProvider>
    </ToastProvider>
  );
}

/** Keyed by pathname: each route resyncs the chrome theme once its surface has painted. */
function BrowserChromeThemeSync() {
  useEffect(() => {
    const frame = window.requestAnimationFrame(() => {
      syncBrowserChromeTheme();
    });
    return () => {
      window.cancelAnimationFrame(frame);
    };
  }, []);

  return null;
}

function DocumentTitleSync() {
  const primaryServerVersion =
    useAtomValue(primaryServerConfigAtom)?.environment.serverVersion ?? null;
  const title = resolveServerBackedAppDisplayName({
    baseName: APP_BASE_NAME,
    fallbackDisplayName: APP_DISPLAY_NAME,
    fallbackStageLabel: APP_STAGE_LABEL,
    primaryServerVersion,
  });

  useEffect(() => {
    document.title = title;
  }, [title]);

  return null;
}

/** Measures client responsiveness for every environment whose profiler is on. */
function ProfilerClientCollector() {
  useEffect(() => startClientCollector(appAtomRegistry), []);
  useEffect(() => startPreviewTunnelBroker(appAtomRegistry), []);
  return null;
}

function HostedStaticEnvironmentBootstrap() {
  const { environments } = useEnvironments();

  useEffect(() => {
    if (
      environments.some(
        (environment) => environment.entry.target._tag === "PrimaryConnectionTarget",
      )
    ) {
      return;
    }

    const firstSavedEnvironment = environments[0];
    if (!firstSavedEnvironment) {
      return;
    }

    reconcileEnvironmentSelection(firstSavedEnvironment.environmentId, "if-unselected");
  }, [environments]);

  return null;
}

function RootRouteErrorView({ error, reset }: ErrorComponentProps) {
  const message = errorMessage(error);
  const details = errorDetails(error);

  return (
    <div className="relative flex min-h-screen items-center justify-center overflow-hidden bg-background px-4 py-10 text-foreground sm:px-6">
      <div className="pointer-events-none absolute inset-0 opacity-80">
        <div className="absolute inset-x-0 top-0 h-44 bg-[radial-gradient(44rem_16rem_at_top,color-mix(in_srgb,var(--color-red-500)_16%,transparent),transparent)]" />
        <div className="absolute inset-0 bg-[linear-gradient(145deg,color-mix(in_srgb,var(--background)_90%,var(--color-black))_0%,var(--background)_55%)]" />
      </div>

      <section className="relative w-full max-w-xl rounded-2xl border border-border/80 bg-card/90 p-6 shadow-2xl shadow-black/20 backdrop-blur-md sm:p-8">
        <p className="text-xs font-semibold tracking-[0.18em] text-muted-foreground uppercase">
          {APP_DISPLAY_NAME}
        </p>
        <h1 className="mt-3 text-2xl font-semibold tracking-tight sm:text-3xl">
          Something went wrong.
        </h1>
        <p className="mt-2 text-sm leading-relaxed text-muted-foreground">{message}</p>

        <div className="mt-5 flex flex-wrap gap-2">
          <Button size="sm" onClick={() => reset()}>
            Try again
          </Button>
          <Button size="sm" variant="outline" onClick={() => window.location.reload()}>
            Reload app
          </Button>
        </div>

        <details className="group mt-5 overflow-hidden rounded-lg border border-border/70 bg-background/55">
          <summary className="cursor-pointer list-none px-3 py-2 text-xs font-medium text-muted-foreground">
            <span className="group-open:hidden">Show error details</span>
            <span className="hidden group-open:inline">Hide error details</span>
          </summary>
          <pre className="max-h-56 overflow-auto border-t border-border/70 bg-background/80 px-3 py-2 text-xs text-foreground/85">
            {details}
          </pre>
        </details>
      </section>
    </div>
  );
}

function errorMessage(error: unknown): string {
  if (error instanceof Error && error.message.trim().length > 0) {
    return error.message;
  }

  if (typeof error === "string" && error.trim().length > 0) {
    return error;
  }

  return "An unexpected router error occurred.";
}

function errorDetails(error: unknown): string {
  const seen = new Set<unknown>();

  const format = (value: unknown): string => {
    if (seen.has(value)) {
      return "[Circular error cause]";
    }
    if ((typeof value === "object" && value !== null) || typeof value === "function") {
      seen.add(value);
    }

    if (value instanceof Error) {
      const ownDetails = value.stack ?? value.message;
      const cause = value.cause;
      return cause === undefined ? ownDetails : `${ownDetails}\n\nCaused by:\n${format(cause)}`;
    }

    if (typeof value === "string") {
      return value;
    }

    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return "No additional error details are available.";
    }
  };

  return format(error);
}

function EventRouter() {
  const navigate = useNavigate();
  const pathname = useLocation({ select: (loc) => loc.pathname });
  const projectGroupingSettings = useClientSettings(selectProjectGroupingSettings);
  const primaryEnvironment = usePrimaryEnvironment();
  const openInEditor = useAtomCommand(shellEnvironment.openInEditor, {
    reportFailure: false,
  });
  const serverConfig = useAtomValue(primaryServerConfigAtom);
  const primaryConfigEnvironmentId = serverConfig?.environment.environmentId ?? null;
  const serverConfigEvent = useAtomValue(primaryServerConfigEventAtom);
  const serverWelcome = useAtomValue(primaryServerWelcomeAtom);
  const readPathname = useEffectEvent(() => pathname);
  const handledBootstrapThreadIdRef = useRef<string | null>(null);
  const handledConfigEventRef = useRef(serverConfigEvent);
  const [keybindingsToastController] = useState<KeybindingsUpdateToastController>(() =>
    createKeybindingsUpdateToastController({}),
  );

  const handleWelcome = useEffectEvent((payload: ServerLifecycleWelcomePayload | null) => {
    if (!payload) return;

    reconcileEnvironmentSelection(payload.environment.environmentId, "if-unselected");
    void (async () => {
      if (!payload.bootstrapProjectId || !payload.bootstrapThreadId) {
        return;
      }
      const bootstrapProject = readProject(
        scopeProjectRef(payload.environment.environmentId, payload.bootstrapProjectId),
      );
      const bootstrapProjectKey =
        (bootstrapProject
          ? deriveLogicalProjectKeyFromSettings(bootstrapProject, projectGroupingSettings)
          : null) ??
        (serverConfig?.cwd
          ? derivePhysicalProjectKeyFromPath(payload.environment.environmentId, serverConfig.cwd)
          : null) ??
        scopedProjectKey(
          scopeProjectRef(payload.environment.environmentId, payload.bootstrapProjectId),
        );
      useUiStateStore.getState().setProjectExpanded(bootstrapProjectKey, true);

      if (
        readPathname() !== "/" ||
        readActiveEnvironmentId() !== payload.environment.environmentId
      ) {
        return;
      }
      if (handledBootstrapThreadIdRef.current === payload.bootstrapThreadId) {
        return;
      }
      await navigate({
        to: "/$environmentId/$threadId",
        params: {
          environmentId: payload.environment.environmentId,
          threadId: payload.bootstrapThreadId,
        },
        replace: true,
      });
      handledBootstrapThreadIdRef.current = payload.bootstrapThreadId;
    })().catch(() => undefined);
  });

  const handleServerConfigUpdated = useEffectEvent(() => {
    const decision = keybindingsToastController.handle(serverConfigEvent);
    if (!decision) {
      return;
    }

    if (decision._tag === "Success") {
      toastManager.add({
        type: "success",
        title: "Keybindings updated",
        description: "Keybindings configuration reloaded successfully.",
      });
      return;
    }

    toastManager.add(
      stackedThreadToast({
        type: "warning",
        title: "Invalid keybindings configuration",
        description: decision.message,
        actionVariant: "outline",
        actionProps: {
          children: "Open keybindings.json",
          onClick: () => {
            if (!serverConfig || !primaryEnvironment) {
              return;
            }

            const editor = resolveAndPersistPreferredEditor(serverConfig.availableEditors);
            if (!editor) {
              return;
            }
            void (async () => {
              const result = await openInEditor({
                environmentId: primaryEnvironment.environmentId,
                input: {
                  cwd: serverConfig.keybindingsConfigPath,
                  editor,
                },
              });
              if (result._tag === "Success") {
                return;
              }
              const error = squashAtomCommandFailure(result);
              toastManager.add(
                stackedThreadToast({
                  type: "error",
                  title: "Unable to open keybindings file",
                  description:
                    error instanceof Error ? error.message : "Unknown error opening file.",
                }),
              );
            })();
          },
        },
      }),
    );
  });

  useEffect(() => {
    if (primaryConfigEnvironmentId === null) {
      return;
    }

    reconcileEnvironmentSelection(primaryConfigEnvironmentId, "follow-primary-identity");
  }, [primaryConfigEnvironmentId]);

  useEffect(() => {
    handleWelcome(serverWelcome);
  }, [serverWelcome]);

  useEffect(() => {
    if (serverConfigEvent === null || handledConfigEventRef.current === serverConfigEvent) {
      return;
    }
    handledConfigEventRef.current = serverConfigEvent;
    handleServerConfigUpdated();
  }, [serverConfigEvent]);

  return null;
}
