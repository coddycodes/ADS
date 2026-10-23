import { act, type ReactElement, type ReactNode } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { createRoot, type Root } from "react-dom/client";
import { Window } from "happy-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import * as Cause from "effect/Cause";
import * as DateTime from "effect/DateTime";
import * as Option from "effect/Option";
import { AsyncResult } from "effect/reactivity";
import {
  type AdvertisedEndpoint,
  type AuthClientSession,
  type AuthEnvironmentScope,
  type AuthPairingLink,
  AuthAccessReadScope,
  AuthAccessWriteScope,
  AuthOrchestrationOperateScope,
  AuthOrchestrationReadScope,
  AuthRelayWriteScope,
  type DesktopDiscoveredSshHost,
  type DesktopWslState,
  EnvironmentId,
} from "@bibcode/contracts";

type AnyProps = Record<string, unknown>;

interface CapturedControl {
  readonly kind: string;
  readonly label: string;
  readonly props: AnyProps;
}

interface MountedTree {
  readonly container: HTMLDivElement;
  readonly root: Root;
}

const mountedTrees: MountedTree[] = [];
let domWindow: Window | null = null;

const h = vi.hoisted(() => {
  const textOf = (node: unknown): string => {
    if (typeof node === "string" || typeof node === "number") return String(node);
    if (Array.isArray(node)) return node.map(textOf).join("");
    if (node !== null && typeof node === "object" && "props" in node) {
      const props = (node as { props: { children?: unknown } }).props;
      return textOf(props.children);
    }
    return "";
  };

  return {
    textOf,
    controls: [] as Array<{ kind: string; label: string; props: Record<string, unknown> }>,
    rows: [] as Array<Record<string, unknown>>,
    stateOverrides: new Map<unknown, unknown>(),
    copyBehavior: "copy" as "copy" | "error",
    copies: [] as Array<{ value: string; context: unknown }>,
    loopbackHostnames: new Set<string>(),
    hasCloudConfig: true,
    connectionsPresentation: "full" as "full" | "local-wsl" | "redirect-general",
    clerkAuth: {
      isSignedIn: true as boolean,
      getToken: vi.fn(),
    },
    environments: [] as unknown[],
    threadShells: [] as unknown[],
    decodedPairingCode: null as null | Record<string, unknown>,
    compatVerdicts: new Map<string, unknown>(),
    primaryEnvironment: null as unknown,
    relayDiscovery: {
      environments: new Map<string, unknown>(),
      refreshing: false,
    },
    primarySessionState: { data: null as unknown },
    cloudLinkState: {
      target: null as unknown,
      data: null as unknown,
      error: null as string | null,
      isPending: false,
      refresh: vi.fn(),
    },
    uiState: {
      defaultAdvertisedEndpointKey: null as string | null,
      setDefaultAdvertisedEndpointKey: vi.fn(),
    },
    nullQuery: {
      data: null as unknown,
      emission: { _tag: "Initial", waiting: false } as unknown,
      error: null as string | null,
      isPending: false,
      refresh: vi.fn(),
      revalidate: vi.fn(),
      requiresRetry: false,
    },
    networkAccessQuery: {
      data: null as unknown,
      error: null as string | null,
      isPending: false,
      refresh: vi.fn(),
    },
    sshHostsQuery: {
      data: null as unknown,
      error: null as string | null,
      isPending: false,
      refresh: vi.fn(),
    },
    wslQuery: {
      data: null as unknown,
      error: null as string | null,
      isPending: false,
      refresh: vi.fn(),
    },
    accessChangesQuery: {
      data: null as unknown,
      error: null as string | null,
      isPending: false,
      refresh: vi.fn(),
    },
    remoteUpdateQueries: new Map<string, Record<string, unknown>>(),
    remoteUpdateCheckStates: new Map<string, unknown>(),
    remoteUpdateRuns: new Map<string, unknown>(),
    updateSnapshotQueries: [] as string[],
    requestRemoteUpdate: vi.fn(),
    dismissRemoteUpdate: vi.fn(),
    atoms: {
      desktopNetworkAccess: Symbol("desktopNetworkAccessStateAtom"),
      desktopSshHosts: Symbol("desktopSshHostsStateAtom"),
      desktopWsl: Symbol("desktopWslStateAtom"),
      connectPairing: Symbol("connectPairing"),
      connectRemoteServer: Symbol("connectRemoteServer"),
      connectSsh: Symbol("connectSshEnvironment"),
      catalogRegister: Symbol("environmentCatalog.register"),
      catalogConnect: Symbol("environmentCatalog.connect"),
      catalogDisconnect: Symbol("environmentCatalog.disconnect"),
      catalogRemove: Symbol("environmentCatalog.remove"),
      catalogRename: Symbol("environmentCatalog.rename"),
      catalogRetryNow: Symbol("environmentCatalog.retryNow"),
      relayRefresh: Symbol("relayEnvironmentDiscovery.refresh"),
      linkPrimary: Symbol("linkPrimaryEnvironment"),
      unlinkPrimary: Symbol("unlinkPrimaryEnvironment"),
      remoteUpdateCheck: Symbol("remoteUpdateEnvironment.check"),
      remoteUpdateInstall: Symbol("remoteUpdateEnvironment.install"),
    },
    commands: {
      connectPairing: vi.fn(),
      connectRemoteServer: vi.fn(),
      connectSsh: vi.fn(),
      register: vi.fn(),
      connect: vi.fn(),
      disconnect: vi.fn(),
      remove: vi.fn(),
      rename: vi.fn(),
      retryNow: vi.fn(),
      relayRefresh: vi.fn(),
      link: vi.fn(),
      unlink: vi.fn(),
      remoteUpdateCheck: vi.fn(),
      remoteUpdateInstall: vi.fn(),
    },
    refreshDesktopNetworkAccessState: vi.fn(),
    refreshDesktopWslState: vi.fn(),
    createServerPairingCredential: vi.fn(),
    revokeServerPairingLink: vi.fn(),
    revokeServerClientSession: vi.fn(),
    revokeOtherServerClientSessions: vi.fn(),
    toastAdd: vi.fn(),
  };
});

// Execute functional state updaters synchronously (React's server renderer
// ignores post-render dispatches) and allow tests to pin selected pieces of
// internal state by their initial value, e.g. forcing the add-environment
// dialog into SSH mode.
vi.mock("react", async (importOriginal) => {
  const actual = await importOriginal<typeof import("react")>();
  const useState = (initial: unknown) => {
    const [value, actualSetState] = (
      actual.useState as (input: unknown) => [unknown, (next: unknown) => void]
    )(initial);
    const isOverridden = typeof initial !== "function" && h.stateOverrides.has(initial);
    const resolved = isOverridden ? h.stateOverrides.get(initial) : value;
    const setState = (next: unknown) => {
      if (isOverridden && typeof next === "function") {
        (next as (previous: unknown) => unknown)(resolved);
        return;
      }
      if (!isOverridden) actualSetState(next);
    };
    return [resolved, setState];
  };
  return { ...actual, useState };
});

vi.mock("@clerk/react", () => ({
  useAuth: () => h.clerkAuth,
}));

vi.mock("@effect/atom-react", () => ({
  useAtomValue: (atom: unknown) => {
    const environmentId = (atom as { environmentId?: string }).environmentId;
    return environmentId === undefined ? null : (h.compatVerdicts.get(environmentId) ?? null);
  },
}));

vi.mock("~/state/session", () => ({
  environmentSession: {
    compatVerdictAtom: (environmentId: string) => ({ environmentId }),
  },
}));

vi.mock("~/connection/currentEnvironmentPresentation", () => ({
  readCurrentEnvironmentPresentationPolicy: () => ({
    connectionsPresentation: h.connectionsPresentation,
  }),
}));

vi.mock("@bibcode/client-runtime/connection", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@bibcode/client-runtime/connection")>()),
}));

vi.mock("@bibcode/client-runtime/errors", () => ({
  findErrorTraceId: (cause: unknown) =>
    cause !== null && typeof cause === "object" && "traceId" in cause
      ? ((cause as { traceId: string }).traceId ?? null)
      : null,
}));

vi.mock("@bibcode/client-runtime/state/runtime", () => ({
  isAtomCommandInterrupted: (result: unknown) =>
    (result as { interrupted?: boolean }).interrupted === true,
  squashAtomCommandFailure: (result: unknown) =>
    (result as { cause?: unknown }).cause ?? new Error("Command failed."),
  settlePromise: async (run: () => Promise<unknown>) => {
    try {
      return { _tag: "Success", value: await run() };
    } catch (cause) {
      return { _tag: "Failure", cause };
    }
  },
}));

vi.mock("../../../hooks/useCopyToClipboard", () => ({
  useCopyToClipboard: (options?: {
    onCopy?: (context: unknown) => void;
    onError?: (error: Error, context: unknown) => void;
  }) => ({
    isCopied: false,
    copyToClipboard: (value: string, context: unknown) => {
      h.copies.push({ value, context });
      if (h.copyBehavior === "error") {
        options?.onError?.(new Error("copy failed"), context);
      } else {
        options?.onCopy?.(context);
      }
    },
  }),
}));

vi.mock("../../../cloud/publicConfig", () => ({
  hasCloudPublicConfig: () => h.hasCloudConfig,
  resolveRelayClerkTokenOptions: () => ({ template: "relay" }),
}));

vi.mock("~/environments/primary", () => ({
  createServerPairingCredential: h.createServerPairingCredential,
  revokeOtherServerClientSessions: h.revokeOtherServerClientSessions,
  revokeServerClientSession: h.revokeServerClientSession,
  revokeServerPairingLink: h.revokeServerPairingLink,
  isLoopbackHostname: (hostname: string) => h.loopbackHostnames.has(hostname),
  usePrimarySessionState: () => h.primarySessionState,
}));

vi.mock("~/connection/desktopLocal", () => ({
  isDesktopLocalConnectionTarget: (target: unknown) =>
    (target as { _tag?: string })._tag === "DesktopLocalConnectionTarget",
}));

vi.mock("~/uiStateStore", () => ({
  useUiStateStore: (selector: (state: unknown) => unknown) => selector(h.uiState),
}));

vi.mock("~/versionSkew", () => ({
  resolveServerConfigVersionMismatch: (serverConfig: unknown) =>
    serverConfig !== null && typeof serverConfig === "object" && "mismatch" in serverConfig
      ? (serverConfig as { mismatch: unknown }).mismatch
      : null,
}));

vi.mock("~/cloud/primaryCloudLinkState", () => ({
  usePrimaryCloudLinkState: () => h.cloudLinkState,
}));

vi.mock("~/cloud/linkEnvironmentAtoms", () => ({
  linkPrimaryEnvironment: h.atoms.linkPrimary,
  unlinkPrimaryEnvironment: h.atoms.unlinkPrimary,
}));

vi.mock("~/state/auth", () => ({
  authEnvironment: {
    accessChanges: (args: unknown) => ({ __kind: "accessChanges", args }),
  },
}));

vi.mock("~/connection/catalog", () => ({
  environmentCatalog: {
    register: h.atoms.catalogRegister,
    connect: h.atoms.catalogConnect,
    disconnect: h.atoms.catalogDisconnect,
    remove: h.atoms.catalogRemove,
    rename: h.atoms.catalogRename,
    retryNow: h.atoms.catalogRetryNow,
  },
}));

vi.mock("~/connection/onboarding", () => ({
  connectPairing: h.atoms.connectPairing,
  connectRemoteServer: h.atoms.connectRemoteServer,
  connectSshEnvironment: h.atoms.connectSsh,
}));

vi.mock("@bibcode/shared/pairingCode", async (importOriginal) => ({
  ...(await importOriginal<typeof import("@bibcode/shared/pairingCode")>()),
  parsePairingCode: () => {
    if (h.decodedPairingCode === null) throw new Error("invalid pairing code");
    return h.decodedPairingCode;
  },
}));

vi.mock("@bibcode/shared/advertisedEndpoint", () => ({
  classifyPairingEndpoint: (endpoint: string) =>
    endpoint.includes("127.0.0.1") || endpoint.includes("localhost") ? "loopback" : "public",
}));

vi.mock("~/state/query", () => ({
  useEnvironmentQuery: (atom: unknown) => {
    if (atom === null) return h.nullQuery;
    if (atom === h.atoms.desktopNetworkAccess) return h.networkAccessQuery;
    if (atom === h.atoms.desktopSshHosts) return h.sshHostsQuery;
    if (atom === h.atoms.desktopWsl) return h.wslQuery;
    if ((atom as { __kind?: string }).__kind === "accessChanges") return h.accessChangesQuery;
    if ((atom as { __kind?: string }).__kind === "remoteUpdateSnapshot") {
      h.updateSnapshotQueries.push((atom as { environmentId: string }).environmentId);
      return (
        h.remoteUpdateQueries.get((atom as { environmentId: string }).environmentId) ?? h.nullQuery
      );
    }
    return h.nullQuery;
  },
}));

vi.mock("~/state/desktopNetworkAccess", () => ({
  desktopNetworkAccessStateAtom: h.atoms.desktopNetworkAccess,
  refreshDesktopNetworkAccessState: h.refreshDesktopNetworkAccessState,
}));

vi.mock("~/state/desktopSshHosts", () => ({
  desktopSshHostsStateAtom: h.atoms.desktopSshHosts,
}));

vi.mock("~/state/desktopWslState", () => ({
  desktopWslStateAtom: h.atoms.desktopWsl,
  refreshDesktopWslState: h.refreshDesktopWslState,
}));

vi.mock("~/state/remoteUpdates", () => ({
  remoteUpdateEnvironment: {
    snapshot: ({ environmentId }: { environmentId: string }) => ({
      __kind: "remoteUpdateSnapshot",
      environmentId,
    }),
    check: h.atoms.remoteUpdateCheck,
    install: h.atoms.remoteUpdateInstall,
  },
  useRemoteUpdateCheckState: (environmentId: string | null) =>
    (environmentId === null ? undefined : h.remoteUpdateCheckStates.get(environmentId)) ?? {
      inFlight: false,
      failure: null,
    },
  useRemoteUpdateRun: (environmentId: string | null) =>
    environmentId === null ? null : (h.remoteUpdateRuns.get(environmentId) ?? null),
  useRequestRemoteUpdateConfirmation: () => h.requestRemoteUpdate,
  useDismissRemoteUpdate: () => h.dismissRemoteUpdate,
}));

vi.mock("~/state/environments", () => ({
  useEnvironments: () => ({ environments: h.environments }),
  usePrimaryEnvironment: () => h.primaryEnvironment,
  useRelayEnvironmentDiscovery: () => h.relayDiscovery,
}));

vi.mock("~/state/entities", () => ({
  useThreadShells: () => h.threadShells,
}));

vi.mock("~/state/relay", () => ({
  relayEnvironmentDiscovery: { refresh: h.atoms.relayRefresh },
}));

vi.mock("../../../state/use-atom-command", () => ({
  useAtomCommand: (atom: unknown) => {
    if (atom === h.atoms.connectPairing) return h.commands.connectPairing;
    if (atom === h.atoms.connectRemoteServer) return h.commands.connectRemoteServer;
    if (atom === h.atoms.connectSsh) return h.commands.connectSsh;
    if (atom === h.atoms.catalogRegister) return h.commands.register;
    if (atom === h.atoms.catalogConnect) return h.commands.connect;
    if (atom === h.atoms.catalogDisconnect) return h.commands.disconnect;
    if (atom === h.atoms.catalogRemove) return h.commands.remove;
    if (atom === h.atoms.catalogRename) return h.commands.rename;
    if (atom === h.atoms.catalogRetryNow) return h.commands.retryNow;
    if (atom === h.atoms.relayRefresh) return h.commands.relayRefresh;
    if (atom === h.atoms.linkPrimary) return h.commands.link;
    if (atom === h.atoms.unlinkPrimary) return h.commands.unlink;
    if (atom === h.atoms.remoteUpdateCheck) return h.commands.remoteUpdateCheck;
    if (atom === h.atoms.remoteUpdateInstall) return h.commands.remoteUpdateInstall;
    throw new Error("Unexpected atom command in test");
  },
}));

vi.mock("../settingsLayout", () => ({
  useRelativeTimeTick: () => Date.now(),
  SettingsPageContainer: (props: AnyProps) => (
    <div data-testid="settings-page">{props.children as ReactNode}</div>
  ),
  SettingsSection: (props: AnyProps) => (
    <section data-section-title={typeof props.title === "string" ? props.title : "custom"}>
      {props.icon as ReactNode}
      {props.title as ReactNode}
      {props.headerAction as ReactNode}
      {props.children as ReactNode}
    </section>
  ),
  SettingsRow: (props: AnyProps) => {
    h.rows.push(props);
    return (
      <div data-testid="settings-row">
        {props.title as ReactNode}
        {props.resetAction as ReactNode}
        {props.description as ReactNode}
        {props.status as ReactNode}
        {props.control as ReactNode}
        {props.children as ReactNode}
      </div>
    );
  },
  SettingResetButton: (props: AnyProps) => (
    <button type="button" data-reset-label={String(props.label)} />
  ),
}));

function renderSlot(render: unknown, children: unknown): ReactNode {
  return (
    <span data-slot>
      {render as ReactNode}
      {children as ReactNode}
    </span>
  );
}

vi.mock("../../ui/button", () => ({
  Button: (props: AnyProps) => {
    h.controls.push({
      kind: "button",
      label: (props["aria-label"] as string | undefined) ?? h.textOf(props.children),
      props,
    });
    return (
      <button
        type={(props.type as "button" | "submit" | "reset" | undefined) ?? "button"}
        form={props.form as string | undefined}
        data-variant={String(props.variant)}
        disabled={Boolean(props.disabled)}
        onClick={props.onClick as (() => void) | undefined}
      >
        {props.children as ReactNode}
      </button>
    );
  },
}));

vi.mock("../../ui/input", () => ({
  Input: (props: AnyProps) => {
    h.controls.push({
      kind: "input",
      label: (props.placeholder as string | undefined) ?? (props.type as string | undefined) ?? "",
      props,
    });
    return (
      <input
        aria-label={props["aria-label"] as string | undefined}
        placeholder={props.placeholder as string | undefined}
        readOnly
        defaultValue={props.value as string | undefined}
      />
    );
  },
}));

vi.mock("../../ui/checkbox", () => ({
  Checkbox: (props: AnyProps) => {
    h.controls.push({ kind: "checkbox", label: String(props.checked), props });
    return <span data-checkbox data-checked={String(props.checked)} />;
  },
}));

vi.mock("../../ui/dialog", () => ({
  Dialog: (props: AnyProps) => {
    h.controls.push({ kind: "dialog", label: String(props.open), props });
    return <div data-dialog>{props.children as ReactNode}</div>;
  },
  DialogTrigger: (props: AnyProps) => renderSlot(props.render, props.children),
  DialogPopup: (props: AnyProps) => (
    <div
      data-dialog-popup
      onKeyDown={props.onKeyDown as React.KeyboardEventHandler<HTMLDivElement> | undefined}
    >
      {props.children as ReactNode}
    </div>
  ),
  DialogHeader: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  DialogTitle: (props: AnyProps) => <h2>{props.children as ReactNode}</h2>,
  DialogDescription: (props: AnyProps) => <p>{props.children as ReactNode}</p>,
  DialogPanel: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  DialogFooter: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  DialogClose: (props: AnyProps) => renderSlot(props.render, props.children),
}));

vi.mock("../../ui/alert-dialog", () => ({
  AlertDialog: (props: AnyProps) => {
    h.controls.push({ kind: "alert-dialog", label: String(props.open), props });
    return <div data-alert-dialog>{props.children as ReactNode}</div>;
  },
  AlertDialogPopup: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogHeader: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogTitle: (props: AnyProps) => <h2>{props.children as ReactNode}</h2>,
  AlertDialogDescription: (props: AnyProps) => <p>{props.children as ReactNode}</p>,
  AlertDialogFooter: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  AlertDialogClose: (props: AnyProps) => renderSlot(props.render, props.children),
}));

vi.mock("../../ui/popover", () => ({
  Popover: (props: AnyProps) => <span data-popover>{props.children as ReactNode}</span>,
  PopoverTrigger: (props: AnyProps) => renderSlot(props.render, props.children),
  PopoverPopup: (props: AnyProps) => <div data-popover-popup>{props.children as ReactNode}</div>,
}));

vi.mock("../../ui/menu", () => ({
  Menu: (props: AnyProps) => <span data-menu>{props.children as ReactNode}</span>,
  MenuTrigger: (props: AnyProps) => renderSlot(props.render, props.children),
  MenuPopup: (props: AnyProps) => <div data-menu-popup>{props.children as ReactNode}</div>,
  MenuItem: (props: AnyProps) => {
    h.controls.push({ kind: "menu-item", label: h.textOf(props.children), props });
    return <div data-menu-item>{props.children as ReactNode}</div>;
  },
  MenuGroup: (props: AnyProps) => <div data-menu-group>{props.children as ReactNode}</div>,
  MenuGroupLabel: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  MenuSeparator: () => <hr />,
}));

vi.mock("../../ui/select", () => ({
  Select: (props: AnyProps) => {
    h.controls.push({ kind: "select", label: String(props.value), props });
    return (
      <div data-select data-value={String(props.value)}>
        {props.children as ReactNode}
      </div>
    );
  },
  SelectTrigger: (props: AnyProps) => (
    <div data-select-trigger aria-label={props["aria-label"] as string | undefined}>
      {props.children as ReactNode}
    </div>
  ),
  SelectValue: (props: AnyProps) => <span>{props.children as ReactNode}</span>,
  SelectPopup: (props: AnyProps) => <div data-select-popup>{props.children as ReactNode}</div>,
  SelectItem: (props: AnyProps) => (
    <div data-select-item data-value={String(props.value)}>
      {props.children as ReactNode}
    </div>
  ),
}));

vi.mock("../../ui/switch", () => ({
  Switch: (props: AnyProps) => {
    h.controls.push({
      kind: "switch",
      label: (props["aria-label"] as string | undefined) ?? "",
      props,
    });
    return (
      <span
        data-switch
        aria-label={props["aria-label"] as string | undefined}
        data-checked={String(props.checked)}
        data-disabled={String(Boolean(props.disabled))}
      />
    );
  },
}));

vi.mock("../../ui/toast", () => ({
  toastManager: { add: h.toastAdd },
  stackedThreadToast: (options: unknown) => options,
}));

vi.mock("../../ui/tooltip", () => ({
  Tooltip: (props: AnyProps) => <>{props.children as ReactNode}</>,
  TooltipTrigger: (props: AnyProps) => renderSlot(props.render, props.children),
  TooltipPopup: (props: AnyProps) => <div data-tooltip-popup>{props.children as ReactNode}</div>,
}));

vi.mock("../../ui/textarea", () => ({
  Textarea: (props: AnyProps) => {
    h.controls.push({
      kind: "textarea",
      label: (props.placeholder as string | undefined) ?? String(props.value),
      props,
    });
    return <textarea readOnly defaultValue={props.value as string | undefined} />;
  },
}));

vi.mock("../../ui/qr-code", () => ({
  QRCodeSvg: (props: AnyProps) => <svg data-qr data-value={String(props.value)} />,
}));

vi.mock("../../ui/skeleton", () => ({
  Skeleton: () => <div data-skeleton />,
}));

vi.mock("../../ui/spinner", () => ({
  Spinner: () => <span data-spinner />,
}));

vi.mock("../../ui/scroll-area", () => ({
  ScrollArea: (props: AnyProps) => <div data-scroll-area>{props.children as ReactNode}</div>,
}));

vi.mock("../../ui/empty", () => ({
  Empty: (props: AnyProps) => <div data-empty>{props.children as ReactNode}</div>,
  EmptyDescription: (props: AnyProps) => <p>{props.children as ReactNode}</p>,
  EmptyHeader: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  EmptyMedia: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
  EmptyTitle: (props: AnyProps) => <h3>{props.children as ReactNode}</h3>,
}));

vi.mock("../../ui/group", () => ({
  Group: (props: AnyProps) => <div data-group>{props.children as ReactNode}</div>,
  GroupSeparator: () => <span data-group-separator />,
}));

vi.mock("../../AnimatedHeight", () => ({
  AnimatedHeight: (props: AnyProps) => <div data-animated>{props.children as ReactNode}</div>,
}));

vi.mock("../../ui/collapsible", () => ({
  Collapsible: (props: AnyProps) => <div data-collapsible>{props.children as ReactNode}</div>,
  CollapsibleTrigger: (props: AnyProps) => <button>{props.children as ReactNode}</button>,
  CollapsibleContent: (props: AnyProps) => <div>{props.children as ReactNode}</div>,
}));

import { ConnectTab } from "./ConnectTab";
import { ShareTab } from "./ShareTab";
import { remoteServersSettingsInternals } from "./shared";

const PRIMARY_ID = EnvironmentId.make("environment-primary");
const FUTURE = DateTime.makeUnsafe("2099-01-01T00:00:00.000Z");
const PAST = DateTime.makeUnsafe("2020-01-01T00:00:00.000Z");
const RECENT = DateTime.makeUnsafe("2026-01-01T00:00:00.000Z");

const STANDARD_SCOPES: ReadonlyArray<AuthEnvironmentScope> = [
  AuthOrchestrationReadScope,
  AuthOrchestrationOperateScope,
];
const ADMIN_SCOPES: ReadonlyArray<AuthEnvironmentScope> = [
  ...STANDARD_SCOPES,
  AuthAccessReadScope,
  AuthAccessWriteScope,
  AuthRelayWriteScope,
];

function success<A>(value: A) {
  return { _tag: "Success" as const, value };
}

function failure(cause: unknown, interrupted = false) {
  return { _tag: "Failure" as const, cause, interrupted };
}

function clearRegistries(): void {
  h.controls.length = 0;
  h.rows.length = 0;
  h.copies.length = 0;
}

function render(
  node: ReactElement = (
    <>
      <ConnectTab />
      <ShareTab />
    </>
  ),
): string {
  clearRegistries();
  return renderToStaticMarkup(node);
}

async function mountConnections(
  node: ReactElement = (
    <>
      <ConnectTab />
      <ShareTab />
    </>
  ),
): Promise<HTMLDivElement> {
  clearRegistries();
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  mountedTrees.push({ container, root });
  await act(async () => root.render(node));
  return container;
}

function findControls(kind: string, label: string): CapturedControl[] {
  const exact = h.controls.filter((entry) => entry.kind === kind && entry.label === label);
  if (exact.length > 0) return exact;
  return h.controls.filter((entry) => entry.kind === kind && entry.label.includes(label));
}

function control(kind: string, label: string): CapturedControl {
  const found = findControls(kind, label);
  if (found.length === 0) {
    throw new Error(`No ${kind} control labelled ${label}`);
  }
  return found[0]!;
}

function invoke(entry: CapturedControl, handlerName: string, ...args: unknown[]): unknown {
  const handler = entry.props[handlerName];
  if (typeof handler !== "function") {
    throw new Error(`Control ${entry.label} has no handler ${handlerName}`);
  }
  return (handler as (...input: unknown[]) => unknown)(...args);
}

/** Click the first button with the given label that actually has an onClick handler. */
function clickButton(label: string): void {
  const target = [...findControls("button", label)]
    .toReversed()
    .find((entry) => typeof entry.props.onClick === "function");
  if (!target) {
    throw new Error(`No clickable button labelled ${label}`);
  }
  invoke(target, "onClick");
}

async function flush(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function stubBrowserWindow(options?: {
  readonly hostname?: string;
  readonly secure?: boolean;
  readonly clipboard?: boolean;
}) {
  const hostname = options?.hostname ?? "app.example.com";
  vi.stubGlobal("window", {
    desktopBridge: undefined,
    isSecureContext: options?.secure ?? true,
    location: {
      href: `https://${hostname}/settings`,
      origin: `https://${hostname}`,
      hostname,
      assign: vi.fn(),
    },
  });
  if (options?.clipboard === false) {
    vi.stubGlobal("navigator", {});
  } else {
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn(async () => {}) } });
  }
}

interface DesktopBridgeStub {
  applyServerExposure: ReturnType<typeof vi.fn>;
  setTailscaleServeEnabled: ReturnType<typeof vi.fn>;
  setWslBackendEnabled: ReturnType<typeof vi.fn>;
  setWslDistro: ReturnType<typeof vi.fn>;
  setWslOnly: ReturnType<typeof vi.fn>;
}

function createDesktopBridgeStub(): DesktopBridgeStub {
  const wslState: DesktopWslState = {
    enabled: true,
    distro: "Ubuntu",
    available: true,
    wslOnly: false,
    distros: [
      { name: "Ubuntu", isDefault: true, version: 2 },
      { name: "Debian", isDefault: false, version: 2 },
    ],
    preflightError: null,
  };
  return {
    applyServerExposure: vi.fn(async () => ({})),
    setTailscaleServeEnabled: vi.fn(async () => ({})),
    setWslBackendEnabled: vi.fn(async () => wslState),
    setWslDistro: vi.fn(async () => wslState),
    setWslOnly: vi.fn(async () => wslState),
  };
}

function stubDesktopWindow(): DesktopBridgeStub {
  const bridge = createDesktopBridgeStub();
  vi.stubGlobal("window", {
    desktopBridge: bridge,
    isSecureContext: true,
    location: {
      href: "https://desktop.localhost/settings",
      origin: "https://desktop.localhost",
      hostname: "desktop.localhost",
      assign: vi.fn(),
    },
  });
  vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn(async () => {}) } });
  return bridge;
}

function installMountedDesktopWindow(): DesktopBridgeStub {
  const bridge = createDesktopBridgeStub();
  Object.defineProperty(window, "desktopBridge", {
    configurable: true,
    value: bridge,
  });
  return bridge;
}

function pairingLink(input: {
  readonly id: string;
  readonly label?: string;
  readonly expiresAt?: DateTime.Utc;
  readonly scopes?: ReadonlyArray<AuthEnvironmentScope>;
}): AuthPairingLink {
  return {
    id: input.id,
    credential: `credential-${input.id}`,
    scopes: input.scopes ?? STANDARD_SCOPES,
    subject: `subject-${input.id}`,
    ...(input.label === undefined ? {} : { label: input.label }),
    createdAt: RECENT,
    expiresAt: input.expiresAt ?? FUTURE,
  } as AuthPairingLink;
}

function clientSession(input: {
  readonly sessionId: string;
  readonly current?: boolean;
  readonly connected?: boolean;
  readonly lastConnectedAt?: DateTime.Utc | null;
  readonly label?: string;
  readonly deviceType?: "desktop" | "mobile" | "unknown";
  readonly os?: string;
  readonly browser?: string;
  readonly ipAddress?: string;
  readonly issuedAt?: DateTime.Utc;
}): AuthClientSession {
  return {
    sessionId: input.sessionId,
    subject: `subject-${input.sessionId}`,
    scopes: STANDARD_SCOPES,
    method: "bearer",
    client: {
      deviceType: input.deviceType ?? "desktop",
      ...(input.label === undefined ? {} : { label: input.label }),
      ...(input.os === undefined ? {} : { os: input.os }),
      ...(input.browser === undefined ? {} : { browser: input.browser }),
      ...(input.ipAddress === undefined ? {} : { ipAddress: input.ipAddress }),
    },
    issuedAt: input.issuedAt ?? RECENT,
    expiresAt: FUTURE,
    lastConnectedAt: input.lastConnectedAt === undefined ? PAST : input.lastConnectedAt,
    connected: input.connected ?? false,
    current: input.current ?? false,
  } as unknown as AuthClientSession;
}

function accessSnapshot(input: {
  readonly pairingLinks: ReadonlyArray<AuthPairingLink>;
  readonly clientSessions: ReadonlyArray<AuthClientSession>;
}) {
  return {
    type: "snapshot" as const,
    payload: { pairingLinks: input.pairingLinks, clientSessions: input.clientSessions },
  };
}

function endpoint(input: {
  readonly id: string;
  readonly label: string;
  readonly httpBaseUrl: string;
  readonly status?: AdvertisedEndpoint["status"];
  readonly reachability?: AdvertisedEndpoint["reachability"];
  readonly hostedHttpsApp?: AdvertisedEndpoint["compatibility"]["hostedHttpsApp"];
  readonly isDefault?: boolean;
  readonly providerId?: string;
}): AdvertisedEndpoint {
  return {
    id: input.id,
    label: input.label,
    provider: {
      id: input.providerId ?? "desktop-core",
      label: "Desktop",
      kind: "core",
      isAddon: false,
    },
    httpBaseUrl: input.httpBaseUrl,
    wsBaseUrl: input.httpBaseUrl.replace(/^http/u, "ws"),
    reachability: input.reachability ?? "lan",
    compatibility: {
      hostedHttpsApp: input.hostedHttpsApp ?? "unknown",
      desktopApp: "compatible",
    },
    source: "desktop-core",
    status: input.status ?? "available",
    ...(input.isDefault === undefined ? {} : { isDefault: input.isDefault }),
  };
}

const UP_TO_DATE_SNAPSHOT = {
  serverVersion: "0.4.2",
  latestVersion: null,
  state: "up-to-date",
  error: null,
  support: { installMode: "interactive", reason: "available", installKind: "unknown" },
  downloadPercent: null,
  targetVersion: null,
  installStage: null,
} as const;

function updateCapableConfig() {
  return {
    environment: {
      serverVersion: "0.4.2",
      capabilities: { remoteUpdateControl: true },
    },
  };
}

function settledUpdateQuery(data: unknown) {
  const refresh = vi.fn();
  return {
    data,
    emission: AsyncResult.success(data),
    error: null,
    isPending: false,
    refresh,
    revalidate: refresh,
    requiresRetry: false,
  };
}

interface TestConnection {
  readonly phase: string;
  readonly error?: unknown;
  readonly traceId?: string | null;
}

function environment(input: {
  readonly id: string;
  readonly label: string;
  readonly targetTag?: string;
  readonly connection?: TestConnection;
  readonly relayManaged?: boolean;
  readonly sshTarget?: {
    alias: string;
    hostname: string;
    username: string | null;
    port: number | null;
  };
  readonly serverConfig?: unknown;
}) {
  const serverConfigSource =
    input.serverConfig !== null && typeof input.serverConfig === "object"
      ? (input.serverConfig as Record<string, unknown>)
      : null;
  const descriptorSource =
    serverConfigSource?.environment !== null && typeof serverConfigSource?.environment === "object"
      ? (serverConfigSource.environment as Record<string, unknown>)
      : {};
  const capabilitiesSource =
    descriptorSource.capabilities !== null && typeof descriptorSource.capabilities === "object"
      ? (descriptorSource.capabilities as Record<string, unknown>)
      : {};
  return {
    environmentId: EnvironmentId.make(input.id),
    label: input.label,
    relayManaged: input.relayManaged ?? false,
    connection: {
      traceId: null,
      ...(input.connection ?? { phase: "disconnected" }),
    },
    serverConfig:
      serverConfigSource === null
        ? null
        : {
            ...serverConfigSource,
            environment: {
              ...descriptorSource,
              capabilities: { remoteUpdateControl: false, ...capabilitiesSource },
            },
          },
    entry: {
      target: { _tag: input.targetTag ?? "SavedConnectionTarget", label: input.label },
      profile: input.sshTarget
        ? Option.some({ _tag: "SshConnectionProfile", target: input.sshTarget })
        : Option.none(),
    },
  };
}

function relayEnvironmentEntry(input: {
  readonly id: string;
  readonly label: string;
  readonly availability: "online" | "offline" | "checking" | "error";
  readonly errorMessage?: string;
}) {
  return {
    environment: { environmentId: EnvironmentId.make(input.id), label: input.label },
    availability: input.availability,
    error:
      input.errorMessage === undefined ? Option.none() : Option.some(new Error(input.errorMessage)),
  };
}

let consoleErrorSpy: ReturnType<typeof vi.spyOn>;

beforeEach(() => {
  vi.unstubAllGlobals();
  domWindow = new Window({ url: "https://bibcode.test/" });
  vi.stubGlobal("window", domWindow);
  vi.stubGlobal("document", domWindow.document);
  vi.stubGlobal("navigator", domWindow.navigator);
  vi.stubGlobal("localStorage", domWindow.localStorage);
  vi.stubGlobal("Node", domWindow.Node);
  vi.stubGlobal("Element", domWindow.Element);
  vi.stubGlobal("HTMLElement", domWindow.HTMLElement);
  vi.stubGlobal("Event", domWindow.Event);
  vi.stubGlobal("MouseEvent", domWindow.MouseEvent);
  vi.stubGlobal("KeyboardEvent", domWindow.KeyboardEvent);
  vi.stubGlobal("CustomEvent", domWindow.CustomEvent);
  vi.stubGlobal("customElements", domWindow.customElements);
  vi.stubGlobal("MutationObserver", domWindow.MutationObserver);
  vi.stubGlobal("ResizeObserver", domWindow.ResizeObserver);
  vi.stubGlobal("getComputedStyle", domWindow.getComputedStyle.bind(domWindow));
  vi.stubGlobal("requestAnimationFrame", domWindow.requestAnimationFrame.bind(domWindow));
  vi.stubGlobal("cancelAnimationFrame", domWindow.cancelAnimationFrame.bind(domWindow));
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  clearRegistries();
  h.stateOverrides.clear();
  h.copyBehavior = "copy";
  h.loopbackHostnames.clear();
  h.loopbackHostnames.add("localhost");
  h.hasCloudConfig = true;
  h.connectionsPresentation = "full";
  h.clerkAuth.isSignedIn = true;
  h.clerkAuth.getToken.mockReset();
  h.clerkAuth.getToken.mockResolvedValue("clerk-token");
  h.environments = [];
  h.threadShells = [];
  h.decodedPairingCode = null;
  h.compatVerdicts.clear();
  h.primaryEnvironment = { environmentId: PRIMARY_ID, serverConfig: null };
  h.relayDiscovery = { environments: new Map(), refreshing: false };
  h.primarySessionState = { data: null };
  h.cloudLinkState = {
    target: { environmentId: PRIMARY_ID },
    data: { linked: true },
    error: null,
    isPending: false,
    refresh: vi.fn(),
  };
  h.uiState = {
    defaultAdvertisedEndpointKey: null,
    setDefaultAdvertisedEndpointKey: vi.fn(),
  };
  h.nullQuery = {
    data: null,
    emission: { _tag: "Initial", waiting: false },
    error: null,
    isPending: false,
    refresh: vi.fn(),
    revalidate: vi.fn(),
    requiresRetry: false,
  };
  h.networkAccessQuery = { data: null, error: null, isPending: false, refresh: vi.fn() };
  h.sshHostsQuery = { data: null, error: null, isPending: false, refresh: vi.fn() };
  h.wslQuery = { data: null, error: null, isPending: false, refresh: vi.fn() };
  h.accessChangesQuery = { data: null, error: null, isPending: false, refresh: vi.fn() };
  h.remoteUpdateQueries.clear();
  h.remoteUpdateCheckStates.clear();
  h.remoteUpdateRuns.clear();
  h.updateSnapshotQueries = [];
  h.requestRemoteUpdate.mockReset();
  h.dismissRemoteUpdate.mockReset();
  for (const command of Object.values(h.commands)) {
    command.mockReset();
    command.mockResolvedValue(success(undefined));
  }
  h.refreshDesktopNetworkAccessState.mockReset();
  h.refreshDesktopWslState.mockReset();
  h.createServerPairingCredential.mockReset();
  h.createServerPairingCredential.mockResolvedValue({ id: "new", credential: "new-credential" });
  h.revokeServerPairingLink.mockReset();
  h.revokeServerPairingLink.mockResolvedValue(true);
  h.revokeServerClientSession.mockReset();
  h.revokeServerClientSession.mockResolvedValue(true);
  h.revokeOtherServerClientSessions.mockReset();
  h.revokeOtherServerClientSessions.mockResolvedValue(1);
  h.toastAdd.mockReset();
  consoleErrorSpy = vi.spyOn(console, "error").mockImplementation(() => {});
});

afterEach(async () => {
  for (const { root, container } of mountedTrees.splice(0)) {
    await act(async () => root.unmount());
    container.remove();
  }
  document.body.replaceChildren();
  consoleErrorSpy.mockRestore();
  domWindow?.close();
  domWindow = null;
  vi.unstubAllGlobals();
});

describe("Remote Servers deterministic helpers", () => {
  it("parses manual SSH targets and rejects every invalid boundary", () => {
    const { formatDesktopSshTarget, parseManualDesktopSshTarget } = remoteServersSettingsInternals;

    expect(() => parseManualDesktopSshTarget({ host: " ", username: "", port: "" })).toThrow(
      "SSH host or alias is required.",
    );
    expect(
      parseManualDesktopSshTarget({ host: "alice@example.test:2222", username: "", port: "" }),
    ).toEqual({ alias: "example.test", hostname: "example.test", username: "alice", port: 2222 });
    expect(
      parseManualDesktopSshTarget({
        host: "inline@example.test",
        username: "explicit",
        port: "2200",
      }),
    ).toEqual({
      alias: "example.test",
      hostname: "example.test",
      username: "explicit",
      port: 2200,
    });
    expect(parseManualDesktopSshTarget({ host: "[::1]", username: "", port: "" })).toEqual({
      alias: "::1",
      hostname: "::1",
      username: null,
      port: null,
    });
    expect(
      parseManualDesktopSshTarget({ host: "host:not-a-port", username: "", port: "" }),
    ).toMatchObject({ hostname: "host:not-a-port", port: null });
    expect(() => parseManualDesktopSshTarget({ host: "user@", username: "", port: "" })).toThrow(
      "SSH host or alias is required.",
    );
    for (const port of ["nope", "0", "65536"]) {
      expect(() =>
        parseManualDesktopSshTarget({ host: "example.test", username: "", port }),
      ).toThrow("SSH port must be between 1 and 65535.");
    }
    expect(
      formatDesktopSshTarget({ alias: "host", hostname: "host", username: null, port: null }),
    ).toBe("host");
    expect(
      formatDesktopSshTarget({ alias: "host", hostname: "host", username: "alice", port: 22 }),
    ).toBe("alice@host:22");
  });

  it("parses pairing URLs through the shared owner and validates plain fields", () => {
    const { parseRemotePairingFields } = remoteServersSettingsInternals;
    expect(
      parseRemotePairingFields({
        host: "https://backend.test/pair?token=url-token",
        pairingCode: "ignored",
      }),
    ).toEqual({ host: "https://backend.test/", pairingCode: "url-token" });
    expect(
      parseRemotePairingFields({ host: "backend.test/pair?token=secret", pairingCode: "" }),
    ).toEqual({ host: "https://backend.test/", pairingCode: "secret" });

    // The shared owner guards the reachable Connect-tab path: a pairing URL
    // carrying userinfo is rejected instead of displaying one host while
    // connecting to another, and unsupported protocols are refused.
    expect(() =>
      parseRemotePairingFields({
        host: "https://trusted.example@attacker.example/pair?token=secret",
        pairingCode: "",
      }),
    ).toThrow("Pairing URL is invalid.");
    expect(() =>
      parseRemotePairingFields({ host: "ftp://backend.test/pair?token=x", pairingCode: "" }),
    ).toThrow("Pairing URL is invalid.");

    // Bare hosts — including malformed ones — defer to the plain fields; the
    // owner still normalizes them at the next step of the connect flow.
    expect(parseRemotePairingFields({ host: " backend.test ", pairingCode: " code " })).toEqual({
      host: "backend.test",
      pairingCode: "code",
    });
    expect(parseRemotePairingFields({ host: "not a valid host", pairingCode: "code" })).toEqual({
      host: "not a valid host",
      pairingCode: "code",
    });
    expect(() => parseRemotePairingFields({ host: "", pairingCode: "code" })).toThrow(
      "Enter a backend host.",
    );
    expect(() => parseRemotePairingFields({ host: "backend.test", pairingCode: "" })).toThrow(
      "Enter a pairing code.",
    );
  });

  it("normalizes SSH errors, timestamps, endpoint classes, and preference keys", () => {
    const {
      endpointDefaultPreferenceKey,
      endpointRowClassName,
      formatAccessTimestamp,
      formatDesktopSshConnectionError,
      isHostedAppPairingUrl,
      isTailscaleHttpsEndpoint,
    } = remoteServersSettingsInternals;
    expect(formatAccessTimestamp("not-a-date")).toBe("not-a-date");
    expect(formatAccessTimestamp("2025-01-01T12:00:00.000Z")).not.toBe("2025-01-01T12:00:00.000Z");
    expect(formatDesktopSshConnectionError("opaque")).toBe("Failed to connect SSH host.");
    expect(
      formatDesktopSshConnectionError(
        new Error(
          "Error invoking remote method 'desktop:ensure-ssh-environment': SshAuthError: denied",
        ),
      ),
    ).toBe("denied");
    expect(formatDesktopSshConnectionError(new Error(" "))).toBe("Failed to connect SSH host.");
    expect(endpointRowClassName("endpoint-rail", false)).toContain("bg-muted/20");
    expect(endpointRowClassName("endpoint-rail", true)).not.toContain("bg-muted/20");
    expect(endpointRowClassName("current", false)).toContain("bg-muted/24");
    expect(endpointRowClassName("current", true)).not.toContain("bg-muted/24");

    const cases = [
      [
        endpoint({ id: "desktop-loopback:1", label: "Loopback", httpBaseUrl: "http://localhost" }),
        "desktop-core:loopback:http",
      ],
      [
        endpoint({ id: "desktop-lan:1", label: "LAN", httpBaseUrl: "http://lan.test" }),
        "desktop-core:lan:http",
      ],
      [
        endpoint({ id: "tailscale-ip:1", label: "Tailnet", httpBaseUrl: "http://100.64.0.1" }),
        "tailscale:ip:http",
      ],
      [
        endpoint({
          id: "tailscale-magicdns:1",
          label: "MagicDNS",
          httpBaseUrl: "https://host.ts.net",
        }),
        "tailscale:magicdns:https",
      ],
    ] as const;
    for (const [candidate, key] of cases) {
      expect(endpointDefaultPreferenceKey(candidate)).toBe(key);
    }
    expect(
      endpointDefaultPreferenceKey(
        endpoint({
          id: "custom:1",
          label: "Custom",
          httpBaseUrl: "https://custom.test",
          providerId: "custom",
        }),
      ),
    ).toBe("custom:lan:https:Custom");
    expect(
      endpointDefaultPreferenceKey(
        endpoint({
          id: "custom:bad",
          label: "Broken",
          httpBaseUrl: "://bad",
          providerId: "custom",
        }),
      ),
    ).toBe("custom:lan:unknown:Broken");
    expect(isTailscaleHttpsEndpoint(cases[3][0])).toBe(true);
    expect(isTailscaleHttpsEndpoint(cases[0][0])).toBe(false);
    expect(isHostedAppPairingUrl("https://app.test/pair?host=https%3A%2F%2Fbackend.test")).toBe(
      true,
    );
    expect(isHostedAppPairingUrl("https://app.test/pair")).toBe(false);
    expect(isHostedAppPairingUrl("not a url")).toBe(false);
  });

  it("selects, sorts, and converts access records across fallback paths", () => {
    const {
      resolveAdvertisedEndpointPairingUrl,
      selectPairingEndpoint,
      sortDesktopClientSessions,
      sortDesktopPairingLinks,
      toDesktopClientSessionRecord,
      toDesktopPairingLinkRecord,
    } = remoteServersSettingsInternals;
    const loopback = endpoint({
      id: "desktop-loopback:1",
      label: "Loopback",
      httpBaseUrl: "http://localhost:9876",
      reachability: "loopback",
    });
    const lan = endpoint({ id: "desktop-lan:1", label: "LAN", httpBaseUrl: "http://lan.test" });
    const hosted = endpoint({
      id: "custom:hosted",
      label: "Hosted",
      httpBaseUrl: "https://hosted.test",
      reachability: "loopback",
      hostedHttpsApp: "compatible",
    });
    const unavailable = endpoint({
      id: "custom:off",
      label: "Offline",
      httpBaseUrl: "https://off.test",
      status: "unavailable",
      isDefault: true,
    });
    expect(selectPairingEndpoint([loopback, lan], "desktop-core:lan:http")).toBe(lan);
    expect(selectPairingEndpoint([{ ...loopback, isDefault: true }, lan], "missing")).toMatchObject(
      {
        id: loopback.id,
      },
    );
    expect(selectPairingEndpoint([loopback, lan])).toBe(lan);
    expect(selectPairingEndpoint([loopback, hosted])).toBe(hosted);
    expect(selectPairingEndpoint([unavailable])).toBeNull();
    expect(resolveAdvertisedEndpointPairingUrl(lan, "credential")).toContain("credential");
    expect(resolveAdvertisedEndpointPairingUrl(hosted, "credential")).toContain("credential");

    const links = [pairingLink({ id: "old", expiresAt: RECENT }), pairingLink({ id: "new" })];
    const linkRecords = links.map(toDesktopPairingLinkRecord);
    expect(
      sortDesktopPairingLinks([
        { ...linkRecords[0]!, createdAt: "2024-01-01T00:00:00.000Z" },
        { ...linkRecords[1]!, createdAt: "2025-01-01T00:00:00.000Z" },
      ])[0]?.id,
    ).toBe("new");
    const sessions = [
      clientSession({ sessionId: "old", issuedAt: PAST }),
      clientSession({ sessionId: "connected", connected: true }),
      clientSession({ sessionId: "current", current: true }),
      clientSession({ sessionId: "new" }),
    ];
    expect(
      sortDesktopClientSessions(sessions.map(toDesktopClientSessionRecord)).map(
        (entry) => entry.sessionId,
      ),
    ).toEqual(["current", "connected", "new", "old"]);
    expect(toDesktopPairingLinkRecord(links[0]!).createdAt).toBe(DateTime.formatIso(RECENT));
    expect(
      toDesktopClientSessionRecord(clientSession({ sessionId: "never", lastConnectedAt: null }))
        .lastConnectedAt,
    ).toBeNull();
    expect(
      toDesktopClientSessionRecord(clientSession({ sessionId: "seen", lastConnectedAt: RECENT }))
        .lastConnectedAt,
    ).toBe(DateTime.formatIso(RECENT));
  });
});

describe("Remote Servers tabs", () => {
  it("shows the administrative-access notice for non-admin browser sessions", () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.primarySessionState = {
      data: { authenticated: true, scopes: STANDARD_SCOPES, auth: { policy: "loopback-only" } },
    };

    const markup = render();

    expect(markup).toContain("Administrative access");
    expect(markup).toContain("access:write");
    expect(markup).toContain("No saved remote environments");
    expect(markup).not.toContain("Authorized clients");
    expect(markup).not.toContain("BiBCode Connect.");
  });

  it("wraps a long connection error instead of cutting off its action", () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.primarySessionState = {
      data: { authenticated: false, auth: { policy: "loopback-only" } },
    };
    h.environments = [
      environment({
        id: "environment-ssh",
        label: "Devbox",
        targetTag: "SshConnectionTarget",
        sshTarget: { alias: "devbox", hostname: "devbox.internal", username: "dev", port: 2222 },
        connection: {
          phase: "error",
          error: new Error(
            "devbox rejected a new pairing credential. Connect again; if it keeps failing, remove the environment and add it again.",
          ),
          traceId: "trace-9",
        },
      }),
    ];

    const markup = render();

    const errorLine =
      /<span class="([^"]*)">[^<]*remove the environment and add it again\.<\/span>/u.exec(markup);
    expect(errorLine, "the error line renders the whole message").not.toBeNull();
    const classes = errorLine![1]!.split(/\s+/u);
    expect(classes).not.toContain("truncate");
    expect(classes).toContain("line-clamp-3");
    expect(markup).toContain("Copy trace ID");
  });

  it.each(["connecting", "reconnecting"])("shows a visible slow-setup status while %s", (phase) => {
    stubBrowserWindow();
    const notice = "Receiving settings from Devbox over a slow connection…";
    const connection = { phase, notice };
    h.environments = [environment({ id: "environment-ssh", label: "Devbox", connection })];

    const container = document.createElement("div");
    container.innerHTML = render(<ConnectTab />);

    const noticeLine = container.querySelector('[role="status"]');
    expect(noticeLine, "the notice renders outside the tooltip").not.toBeNull();
    expect(noticeLine?.closest("[data-tooltip-popup]")).toBeNull();
    expect(noticeLine?.textContent).toBe(notice);
    expect(noticeLine?.getAttribute("aria-live")).toBe("polite");
    expect(noticeLine?.classList.contains("text-xs")).toBe(true);
    expect(noticeLine?.classList.contains("text-muted-foreground")).toBe(true);
    expect(noticeLine?.classList.contains("text-destructive")).toBe(false);
    expect(noticeLine?.classList.contains("flex-wrap")).toBe(true);
    expect(noticeLine?.querySelector(".truncate")).toBeNull();
    expect(noticeLine?.querySelector(".wrap-break-word")).not.toBeNull();
    expect(noticeLine?.querySelector(".animate-spin")).not.toBeNull();
    expect(noticeLine?.querySelector('svg[aria-hidden="true"]')).not.toBeNull();
    expect(container.textContent).not.toContain("Copy trace ID");
  });

  it.each(["error", "reconnecting"])(
    "shows only the error line when a notice is also present while %s",
    (phase) => {
      stubBrowserWindow();
      const notice = "Receiving settings from Devbox over a slow connection…";
      const connection = {
        phase,
        error: "Devbox did not send its settings. Connect again.",
        notice,
        traceId: "trace-9",
      };
      h.environments = [environment({ id: "environment-ssh", label: "Devbox", connection })];

      const container = document.createElement("div");
      container.innerHTML = render(<ConnectTab />);
      for (const tooltip of container.querySelectorAll("[data-tooltip-popup]")) tooltip.remove();

      const errorLines = container.querySelectorAll("p.text-destructive");
      expect(errorLines).toHaveLength(1);
      expect(errorLines[0]?.textContent).toContain(connection.error);
      expect(errorLines[0]?.textContent).toContain("Copy trace ID");
      expect(container.querySelector('[role="status"]')).toBeNull();
      expect(container.textContent).not.toContain(notice);
    },
  );

  it("shows no connection status line when connected without an error or notice", () => {
    stubBrowserWindow();
    h.environments = [
      environment({
        id: "environment-ssh",
        label: "Devbox",
        connection: { phase: "connected" },
      }),
    ];

    const container = document.createElement("div");
    container.innerHTML = render(<ConnectTab />);

    expect(container.querySelector('[role="status"]')).toBeNull();
    expect(container.querySelector("p.text-destructive")).toBeNull();
    expect(container.textContent).not.toContain("Copy trace ID");
  });

  it("lists saved environments with connect, disconnect and error affordances", async () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.primarySessionState = {
      data: { authenticated: false, auth: { policy: "loopback-only" } },
    };
    h.environments = [
      environment({
        id: "environment-ssh",
        label: "Devbox",
        targetTag: "SshConnectionTarget",
        connection: { phase: "connected" },
        sshTarget: { alias: "devbox", hostname: "devbox.internal", username: "dev", port: 2222 },
        serverConfig: { mismatch: { clientVersion: "1.0.0", serverVersion: "2.0.0" } },
      }),
      environment({
        id: "environment-relay",
        label: "Relay Env",
        targetTag: "RelayConnectionTarget",
        relayManaged: true,
        connection: { phase: "error", error: new Error("boom"), traceId: "trace-1" },
      }),
      environment({
        id: "environment-wsl",
        label: "WSL Backend",
        targetTag: "DesktopLocalConnectionTarget",
        connection: { phase: "connecting" },
      }),
      environment({
        id: "environment-idle",
        label: "Idle Env",
        connection: { phase: "available" },
      }),
    ];

    const markup = render();

    expect(markup).toContain("Devbox");
    expect(markup).toContain("SSH dev@devbox.internal:2222");
    expect(markup).toContain("Version drift: client 1.0.0, server 2.0.0.");
    expect(markup).toContain("Connection failed. Reason: Error: boom");
    expect(markup).toContain("Copy trace ID");
    expect(markup).toContain("Managed above");
    expect(markup).not.toContain("No saved remote environments");

    // Disconnect the connected SSH environment.
    invoke(control("button", "Disconnect"), "onClick");
    await flush();
    expect(h.commands.disconnect).toHaveBeenCalledWith(EnvironmentId.make("environment-ssh"));
    expect(h.commands.remove).not.toHaveBeenCalled();

    // Connect the idle environment.
    invoke(control("button", "Connect"), "onClick");
    await flush();
    expect(h.commands.connect).toHaveBeenCalledWith(EnvironmentId.make("environment-idle"));

    // Failures surface a toast.
    h.commands.connect.mockResolvedValueOnce(failure(new Error("connect failed")));
    invoke(control("button", "Connect"), "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not connect backend" }),
    );

    h.commands.disconnect.mockResolvedValueOnce(failure(new Error("disconnect failed")));
    invoke(control("button", "Disconnect"), "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not disconnect backend" }),
    );
  });

  it("Disconnect latches instead of removing, and Remove moves behind a confirmation", async () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.environments = [
      environment({
        id: "env-1",
        label: "AI-SERVER",
        targetTag: "BearerConnectionTarget",
        connection: { phase: "connected" },
      }),
    ];

    const container = await mountConnections();
    invoke(control("button", "Disconnect"), "onClick");
    await flush();
    expect(h.commands.disconnect).toHaveBeenCalledWith(EnvironmentId.make("env-1"));
    expect(h.commands.remove).not.toHaveBeenCalled();

    await act(async () => {
      invoke(control("menu-item", "Remove server…"), "onClick");
    });
    expect(container.textContent).toContain("Remove AI-SERVER?");
    expect(h.commands.remove).not.toHaveBeenCalled();

    await act(async () => {
      invoke(findControls("button", "Remove server").at(-1)!, "onClick");
      await flush();
    });
    expect(h.commands.remove).toHaveBeenCalledWith(EnvironmentId.make("env-1"));
  });

  it("renders a latched available environment as Disconnected with a Connect action", () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.environments = [
      environment({
        id: "env-1",
        label: "AI-SERVER",
        targetTag: "BearerConnectionTarget",
        connection: { phase: "available" },
      }),
    ];

    const markup = render(<ConnectTab />);
    expect(markup).toContain("Disconnected");
    expect(markup).toContain("Connect");
  });

  it("escalates removal confirmation when the environment owns running work", async () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.environments = [
      environment({
        id: "env-1",
        label: "AI-SERVER",
        targetTag: "BearerConnectionTarget",
        connection: { phase: "connected" },
      }),
    ];
    h.threadShells = [
      { environmentId: EnvironmentId.make("env-1"), session: { status: "running" } },
      { environmentId: EnvironmentId.make("env-1"), session: { status: "running" } },
      { environmentId: EnvironmentId.make("env-2"), session: { status: "running" } },
    ];

    const container = await mountConnections();
    await act(async () => {
      invoke(control("menu-item", "Remove server…"), "onClick");
    });
    expect(container.textContent).toContain("2 running sessions");
    expect(h.commands.remove).not.toHaveBeenCalled();
  });

  describe("Rename flow", () => {
    const serverNameInput = () =>
      h.controls.findLast(
        (entry) => entry.kind === "input" && entry.props["aria-label"] === "Server name",
      )!;
    const saveButton = () => findControls("button", "Save").at(-1)!;
    const buttonIn = (container: HTMLElement, label: string) =>
      Array.from(container.querySelectorAll("button")).find(
        (button) => button.textContent === label,
      )!;

    function renderRenamableServer() {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      h.environments = [
        environment({
          id: "env-1",
          label: "ai-server",
          targetTag: "BearerConnectionTarget",
          connection: { phase: "connected" },
          serverConfig: { environment: { label: "ai-server" } },
        }),
      ];
      return mountConnections(<ConnectTab />);
    }

    it("renames a saved server from its row menu with the current name prefilled", async () => {
      const container = await renderRenamableServer();
      expect(container.textContent).not.toContain("Rename server");

      await act(async () => {
        invoke(control("menu-item", "Rename…"), "onClick");
      });
      expect(container.textContent).toContain("Rename server");
      expect(container.textContent).toContain("The new name shows on this device only.");
      expect(serverNameInput().props.value).toBe("ai-server");
      // The server's own name is the current name, so there is nothing to restore.
      expect(container.textContent).not.toContain("Use the server’s name");

      await act(async () => {
        invoke(serverNameInput(), "onChange", { target: { value: "  GPU box " } });
      });
      await act(async () => {
        buttonIn(container, "Save").click();
        await flush();
      });

      expect(h.commands.rename).toHaveBeenCalledExactlyOnceWith({
        environmentId: EnvironmentId.make("env-1"),
        label: "GPU box",
      });
      expect(h.commands.remove).not.toHaveBeenCalled();
      expect(container.textContent).not.toContain("Rename server");
    });

    it("associates Save with the name field's form and saves through requestSubmit", async () => {
      const container = await renderRenamableServer();
      await act(async () => {
        invoke(control("menu-item", "Rename…"), "onClick");
      });
      await act(async () => {
        invoke(serverNameInput(), "onChange", { target: { value: "  GPU box  " } });
      });
      const input = container.querySelector<HTMLInputElement>('[aria-label="Server name"]')!;
      const save = buttonIn(container, "Save");
      const form = input.form!;
      expect(form.id).not.toBe("");
      expect(save.getAttribute("form")).toBe(form.id);
      expect(save.form).toBe(form);
      await act(async () => {
        input.focus();
        const submit = input.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
        );
        // Happy DOM does not synthesize Enter's implicit form submission.
        if (submit) form.requestSubmit(save);
        await flush();
      });
      expect(h.commands.rename).toHaveBeenCalledExactlyOnceWith({
        environmentId: EnvironmentId.make("env-1"),
        label: "GPU box",
      });
      expect(container.textContent).not.toContain("Rename server");
    });

    it("explains a blank name and offers the server's own name back", async () => {
      const container = await renderRenamableServer();
      await act(async () => {
        invoke(control("menu-item", "Rename…"), "onClick");
      });

      await act(async () => {
        invoke(serverNameInput(), "onChange", { target: { value: " \t " } });
      });
      expect(container.textContent).toContain("Enter a name for this server.");
      expect(saveButton().props.disabled).toBe(true);

      await act(async () => {
        invoke(findControls("button", "Use the server’s name").at(-1)!, "onClick");
      });
      expect(serverNameInput().props.value).toBe("ai-server");
      expect(container.textContent).not.toContain("Enter a name for this server.");
      expect(saveButton().props.disabled).toBe(false);
    });

    it.each(["escape-key", "outside-press"])(
      "keeps the rename dialog open during saving on %s",
      async (reason) => {
        const container = await renderRenamableServer();
        let finish!: (value: ReturnType<typeof success>) => void;
        h.commands.rename.mockImplementationOnce(
          () =>
            new Promise((resolve) => {
              finish = resolve;
            }),
        );
        await act(async () => {
          invoke(control("menu-item", "Rename…"), "onClick");
        });
        await act(async () => {
          invoke(serverNameInput(), "onChange", { target: { value: "GPU box" } });
        });
        await act(async () => {
          buttonIn(container, "Save").click();
        });
        expect(h.commands.rename).toHaveBeenCalledOnce();
        expect(container.textContent).toContain("Saving…");
        await act(async () => {
          invoke(findControls("dialog", "true").at(-1)!, "onOpenChange", false, {
            reason,
            event: new KeyboardEvent("keydown", { key: "Escape" }),
          });
        });
        expect(container.textContent).toContain("Rename server");
        expect(serverNameInput().props.value).toBe("GPU box");
        await act(async () => {
          finish(success(undefined));
          await flush();
        });
        expect(container.textContent).not.toContain("Rename server");
      },
    );

    it("does not save when Enter is pressed on Cancel", async () => {
      const container = await renderRenamableServer();
      await act(async () => {
        invoke(control("menu-item", "Rename…"), "onClick");
      });
      await act(async () => {
        invoke(serverNameInput(), "onChange", { target: { value: "GPU box" } });
      });
      const cancel = buttonIn(container, "Cancel");
      await act(async () => {
        cancel.focus();
        cancel.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
        );
      });
      expect(h.commands.rename).not.toHaveBeenCalled();
      // Happy DOM does not synthesize the browser's keyboard activation click.
      await act(async () => {
        cancel.click();
      });
      expect(container.textContent).not.toContain("Rename server");
    });

    it("uses the server name without saving when its control is activated with Enter", async () => {
      const container = await renderRenamableServer();
      await act(async () => {
        invoke(control("menu-item", "Rename…"), "onClick");
      });
      await act(async () => {
        invoke(serverNameInput(), "onChange", { target: { value: "GPU box" } });
      });
      const restore = buttonIn(container, "Use the server’s name: ai-server");
      await act(async () => {
        restore.focus();
        restore.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true }),
        );
        restore.click();
      });
      expect(h.commands.rename).not.toHaveBeenCalled();
      expect(serverNameInput().props.value).toBe("ai-server");
      expect(container.textContent).toContain("Rename server");
    });

    it.each([
      {
        error: new Error("Catalog unavailable"),
        message: "Couldn't save the name on this device. Try again.",
      },
      {
        error: { _tag: "EnvironmentNotRegisteredError", environmentId: "env-1" },
        message: "This server is no longer saved on this device.",
      },
      {
        error: {
          _tag: "ConnectionBlockedError",
          message: "This device names its own environments.",
        },
        message: "This device names its own environments.",
      },
    ])(
      "describes a rejected save as $message and preserves the name for retry",
      async ({ error, message }) => {
        const container = await renderRenamableServer();
        h.commands.rename.mockRejectedValueOnce(error);
        await act(async () => {
          invoke(control("menu-item", "Rename…"), "onClick");
        });
        await act(async () => {
          invoke(serverNameInput(), "onChange", { target: { value: "GPU box" } });
        });
        await act(async () => {
          buttonIn(container, "Save").click();
          await flush();
        });
        expect(container.textContent).toContain(message);
        expect(serverNameInput().props.value).toBe("GPU box");
        expect(saveButton().props.disabled).toBe(false);
        await act(async () => {
          buttonIn(container, "Save").click();
          await flush();
        });
        expect(container.textContent).not.toContain("Rename server");
      },
    );

    it("keeps the dialog and the typed name when saving fails", async () => {
      const container = await renderRenamableServer();
      h.commands.rename.mockResolvedValueOnce(
        failure({ _tag: "EnvironmentNotRegisteredError", environmentId: "env-1" }),
      );
      await act(async () => {
        invoke(control("menu-item", "Rename…"), "onClick");
      });
      await act(async () => {
        invoke(serverNameInput(), "onChange", { target: { value: "GPU box" } });
      });
      await act(async () => {
        buttonIn(container, "Save").click();
        await flush();
      });

      expect(container.textContent).toContain("Rename server");
      expect(container.textContent).toContain("This server is no longer saved on this device.");
      expect(serverNameInput().props.value).toBe("GPU box");

      await act(async () => {
        clickButton("Cancel");
      });
      expect(container.textContent).not.toContain("Rename server");
      expect(h.commands.rename).toHaveBeenCalledOnce();
    });
  });

  describe("Add Server flow", () => {
    async function enterPairingCode(value: string): Promise<void> {
      await act(async () => {
        invoke(control("textarea", "bibcode://pair?code=…"), "onChange", {
          target: { value },
        });
      });
    }

    function latestAddServerButton(): CapturedControl {
      const button = [...findControls("button", "Add Server")]
        .toReversed()
        .find((entry) => typeof entry.props.onClick === "function");
      if (!button) throw new Error("No Add Server submit button");
      return button;
    }

    async function submitAddServer(): Promise<void> {
      await act(async () => {
        invoke(latestAddServerButton(), "onClick");
        await flush();
      });
    }

    async function acknowledgeTunnel(): Promise<void> {
      await act(async () => {
        invoke(findControls("checkbox", "false").at(-1)!, "onCheckedChange", true);
      });
    }

    it("submits a normalized pairing code through connectRemoteServer", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      await mountConnections();
      await enterPairingCode("bibcode://pair?code=abc123");
      await submitAddServer();
      expect(h.commands.connectRemoteServer).toHaveBeenCalledWith({
        code: "abc123",
        allowLoopbackTunnel: false,
      });
    });

    it("keeps the server alias after failed pairing and clears it after success", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      h.commands.connectRemoteServer.mockResolvedValueOnce(
        failure({ _tag: "PairingAddError", reason: "pairing-rejected", detail: "expired" }),
      );
      const container = await mountConnections();
      await enterPairingCode("abc123");
      expect(container.textContent).toContain("Server alias (optional)");
      await act(async () => {
        invoke(control("input", "e.g. Linux workstation"), "onChange", {
          target: { value: "  Linux workstation  " },
        });
      });
      await submitAddServer();
      expect(h.commands.connectRemoteServer).toHaveBeenLastCalledWith({
        code: "abc123",
        allowLoopbackTunnel: false,
        label: "Linux workstation",
      });
      expect(findControls("input", "e.g. Linux workstation").at(-1)?.props.value).toBe(
        "  Linux workstation  ",
      );
      await submitAddServer();
      expect(findControls("input", "e.g. Linux workstation").at(-1)?.props.value).toBe("");
    });

    it("renders classified failure copy for a rejected pairing", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      h.commands.connectRemoteServer.mockResolvedValueOnce(
        failure({ _tag: "PairingAddError", reason: "pairing-rejected", detail: "expired" }),
      );
      const container = await mountConnections();
      await enterPairingCode("abc123");
      await submitAddServer();
      expect(container.textContent).toContain("Pairing rejected");
      expect(container.textContent).toContain("Codes are single-use and expire");
    });

    it("requires tunnel acknowledgement for a this-computer code", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      h.decodedPairingCode = {
        v: 1,
        endpoint: "http://127.0.0.1:3773",
        name: "LOCAL-ONLY",
        token: "t",
        hostKey: "k",
        reach: "this-computer",
        storageInstanceId: "s",
      };
      const container = await mountConnections();
      await enterPairingCode("abc123");
      expect(container.textContent).toContain("only reachable on the server itself");
      expect(latestAddServerButton().props.disabled).toBe(true);
      await acknowledgeTunnel();
      expect(latestAddServerButton().props.disabled).toBe(false);
      await submitAddServer();
      expect(h.commands.connectRemoteServer).toHaveBeenCalledWith({
        code: "abc123",
        allowLoopbackTunnel: true,
      });
    });

    it("reveals acknowledgement when the flow demands it, then retries with it", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      h.commands.connectRemoteServer
        .mockResolvedValueOnce(
          failure({
            _tag: "PairingLoopbackAcknowledgementRequiredError",
            endpoint: "http://127.0.0.1:3773",
          }),
        )
        .mockResolvedValueOnce(success(EnvironmentId.make("env-9")));
      const container = await mountConnections();
      await enterPairingCode("abc123");
      await submitAddServer();
      expect(container.textContent).toContain("only reachable on the server itself");
      await acknowledgeTunnel();
      await submitAddServer();
      expect(h.commands.connectRemoteServer).toHaveBeenLastCalledWith({
        code: "abc123",
        allowLoopbackTunnel: true,
      });
    });

    it("keeps manual entry under Advanced and SSH as a first-class mode", () => {
      stubDesktopWindow();
      const markup = render(<ConnectTab />);
      expect(markup).toContain("Pairing code");
      expect(markup).toContain("SSH");
      expect(markup).toContain("Advanced");
      expect(markup).toContain("Troubleshooting");
    });

    it("consumes an initial pairing code when its prefilled dialog closes", async () => {
      stubBrowserWindow();
      const onPairingCodeConsumed = vi.fn();
      await mountConnections(
        <ConnectTab initialPairingCode="abc123" onPairingCodeConsumed={onPairingCodeConsumed} />,
      );
      await act(async () => {
        invoke(findControls("dialog", "true").at(-1)!, "onOpenChange", false);
      });
      expect(onPairingCodeConsumed).toHaveBeenCalledTimes(1);
    });

    it("opens and consumes the one-shot add-server action", async () => {
      stubBrowserWindow();
      const onAddServerActionConsumed = vi.fn();

      await mountConnections(
        <ConnectTab initialAddServerOpen onAddServerActionConsumed={onAddServerActionConsumed} />,
      );

      expect(findControls("dialog", "true")).not.toHaveLength(0);
      expect(onAddServerActionConsumed).toHaveBeenCalledTimes(1);
    });
  });

  describe("Check for Server Updates placement", () => {
    it("requests confirmation instead of directly installing from Settings", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const id = EnvironmentId.make("update-confirm");
      h.environments = [
        environment({
          id,
          label: "Build server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(
        id,
        settledUpdateQuery({
          ...UP_TO_DATE_SNAPSHOT,
          latestVersion: "0.7.3",
          state: "update-available",
        }),
      );
      await mountConnections(<ConnectTab />);
      await act(async () => {
        clickButton("Update to v0.7.3…");
        await flush();
      });
      expect(h.requestRemoteUpdate).toHaveBeenCalledExactlyOnceWith(
        expect.objectContaining({
          environmentId: id,
          name: "Build server",
          targetVersion: "0.7.3",
        }),
      );
      expect(h.commands.remoteUpdateInstall).not.toHaveBeenCalled();
    });
    it("shows coordinator progress and stops observing its status query while active", () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const id = EnvironmentId.make("update-active");
      h.environments = [
        environment({
          id,
          label: "Build server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateRuns.set(id, {
        phase: "installing",
        stage: "creating-verified-backup",
        targetVersion: "0.7.3",
      });
      const markup = render(<ConnectTab />);
      expect(markup).toContain("Backing up project data…");
      expect(h.updateSnapshotQueries).not.toContain(id);
      expect(control("button", "Check").props.disabled).toBe(true);
    });
    it("shows a failed run with Retry confirmation and Dismiss", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const id = EnvironmentId.make("update-failed");
      h.environments = [
        environment({
          id,
          label: "Build server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(id, settledUpdateQuery(UP_TO_DATE_SNAPSHOT));
      h.remoteUpdateRuns.set(id, {
        phase: "failed",
        failure: { kind: "wrong-version", runningVersion: "0.7.2", targetVersion: "0.7.3" },
      });
      const container = await mountConnections(<ConnectTab />);
      expect(container.textContent).toContain(
        "Build server restarted on v0.7.2 instead of v0.7.3.",
      );
      await act(async () => {
        clickButton("Retry");
        await flush();
      });
      expect(h.requestRemoteUpdate).toHaveBeenCalledOnce();
      expect(h.commands.remoteUpdateInstall).not.toHaveBeenCalled();
      await act(async () => {
        clickButton("Dismiss");
        await flush();
      });
      expect(h.dismissRemoteUpdate).toHaveBeenCalledExactlyOnceWith(id);
    });
    it("is hidden while the Phase 7 capability seam is off", () => {
      stubBrowserWindow();
      expect(render(<ConnectTab />)).not.toContain("Check for Server Updates");
    });

    it("renders in the Saved servers header when the seam is enabled", () => {
      stubBrowserWindow();
      h.environments = [
        environment({
          id: "env-capable",
          label: "Capable server",
          serverConfig: {
            environment: { capabilities: { remoteUpdateControl: true } },
          },
        }),
      ];
      expect(render(<ConnectTab showServerUpdateCheck />)).toContain("Check for Server Updates");
    });

    it("renders update badges only for servers advertising update control", () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const capableId = EnvironmentId.make("env-capable");
      h.environments = [
        environment({ id: "env-legacy", label: "Legacy server" }),
        environment({
          id: capableId,
          label: "Capable server",
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(
        capableId,
        settledUpdateQuery({
          ...UP_TO_DATE_SNAPSHOT,
          state: "idle",
          support: {
            installMode: "manual",
            reason: "manual-update-required",
            installKind: "unknown",
          },
        }),
      );

      const markup = render(<ConnectTab />);
      expect(markup.match(/data-variant="manual"/gu)).toHaveLength(1);
      expect(markup).toContain("Manual updates");
      expect(markup).toContain("Show update steps");
      expect(markup).toContain("Copy");
      const content = document.createElement("div");
      content.innerHTML = markup;
      const steps = content.querySelector("pre");
      expect(steps?.dataset.textSurface).toBe("card");
      expect(steps?.classList.contains("bg-card")).toBe(true);
      expect(steps?.parentElement?.classList.contains("overflow-x-auto")).toBe(true);
    });

    it("shows a failed check as Check failed with its reason, and Check again re-runs it", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-check-failure");
      h.environments = [
        environment({
          id: environmentId,
          label: "Slow update server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(environmentId, settledUpdateQuery(UP_TO_DATE_SNAPSHOT));
      h.remoteUpdateCheckStates.set(environmentId, {
        inFlight: false,
        failure: { cause: Cause.fail(new Error("feed timed out")), generation: 1 },
      });

      const container = await mountConnections(<ConnectTab />);

      expect(container.querySelector('[data-variant="check-failed"]')?.textContent).toContain(
        "Check failed",
      );
      expect(container.textContent).toContain("feed timed out");
      expect(container.textContent).not.toContain("Up to date");
      expect(container.textContent).not.toContain("Can't reach updater");
      // The row's own Check turns into the next step; the badge adds no second button.
      expect(findControls("button", "Retry update status")).toHaveLength(0);
      await act(async () => {
        clickButton("Check again");
        await flush();
      });
      expect(h.commands.remoteUpdateCheck).toHaveBeenCalledExactlyOnceWith({
        environmentId,
        input: {},
      });
    });

    it("offers Retry that re-reads a status read that failed over a live connection", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-status-failure");
      h.environments = [
        environment({
          id: environmentId,
          label: "Flaky update server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      const refresh = vi.fn();
      h.remoteUpdateQueries.set(environmentId, {
        data: null,
        emission: AsyncResult.failure(Cause.fail(new Error("Desktop updater did not answer."))),
        error: "Desktop updater did not answer.",
        isPending: false,
        refresh,
      });

      const container = await mountConnections(<ConnectTab />);

      expect(container.textContent).toContain("Can't reach updater");
      expect(container.textContent).toContain("Desktop updater did not answer.");
      await act(async () => {
        clickButton("Retry update status");
        await flush();
      });
      expect(refresh).toHaveBeenCalledOnce();
      expect(h.commands.remoteUpdateCheck).not.toHaveBeenCalled();
    });

    it("re-reads each status after Check for Server Updates without clearing a cut-off latch", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-fan-out");
      h.environments = [
        environment({
          id: environmentId,
          label: "Fan-out server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      const refresh = vi.fn();
      const revalidate = vi.fn();
      h.remoteUpdateQueries.set(environmentId, {
        ...settledUpdateQuery(UP_TO_DATE_SNAPSHOT),
        refresh,
        revalidate,
        requiresRetry: true,
      });

      await mountConnections(<ConnectTab showServerUpdateCheck />);
      await act(async () => {
        clickButton("Check for Server Updates");
        await flush();
      });

      expect(h.commands.remoteUpdateCheck).toHaveBeenCalledExactlyOnceWith({
        environmentId,
        input: {},
      });
      // The fan-out's epoch is automatic: only the row's Retry may clear the latch.
      expect(revalidate).toHaveBeenCalledOnce();
      expect(refresh).not.toHaveBeenCalled();
    });

    it("re-reads the status after a row Check without clearing a cut-off latch", async () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-row-check");
      h.environments = [
        environment({
          id: environmentId,
          label: "Row check server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      const refresh = vi.fn();
      const revalidate = vi.fn();
      h.remoteUpdateQueries.set(environmentId, {
        ...settledUpdateQuery(UP_TO_DATE_SNAPSHOT),
        refresh,
        revalidate,
        requiresRetry: true,
      });

      await mountConnections(<ConnectTab />);
      await act(async () => {
        clickButton("Check");
        await flush();
      });

      expect(h.commands.remoteUpdateCheck).toHaveBeenCalledExactlyOnceWith({
        environmentId,
        input: {},
      });
      expect(revalidate).toHaveBeenCalledOnce();
      expect(refresh).not.toHaveBeenCalled();
    });

    it("says a host has not checked yet and keeps Check usable during background re-reads", () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-not-checked");
      h.environments = [
        environment({
          id: environmentId,
          label: "Fresh desktop host",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(environmentId, {
        ...settledUpdateQuery({ ...UP_TO_DATE_SNAPSHOT, state: "idle" }),
        isPending: true,
      });

      const markup = render(<ConnectTab />);

      expect(markup).toContain("Not checked yet");
      expect(markup).not.toContain("Status unavailable");
      const check = control("button", "Check");
      expect(check.props.disabled).toBe(false);
    });

    it("shows Checking… while the first status read is in flight", () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-first-read");
      h.environments = [
        environment({
          id: environmentId,
          label: "Loading update server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(environmentId, {
        data: null,
        emission: AsyncResult.initial(true),
        error: null,
        isPending: true,
        refresh: vi.fn(),
      });

      const markup = render(<ConnectTab />);

      expect(markup).toContain('data-variant="checking"');
      expect(control("button", "Checking…").props.disabled).toBe(true);
    });

    it("shows a check started from any view as Checking… on the row", () => {
      stubBrowserWindow();
      h.hasCloudConfig = false;
      const environmentId = EnvironmentId.make("env-check-running");
      h.environments = [
        environment({
          id: environmentId,
          label: "Checking update server",
          connection: { phase: "connected" },
          serverConfig: updateCapableConfig(),
        }),
      ];
      h.remoteUpdateQueries.set(environmentId, settledUpdateQuery(UP_TO_DATE_SNAPSHOT));
      h.remoteUpdateCheckStates.set(environmentId, { inFlight: true, failure: null });

      const markup = render(<ConnectTab />);

      expect(markup).toContain('data-variant="checking"');
      expect(control("button", "Checking…").props.disabled).toBe(true);
    });
  });

  it("manages pairing links and paired clients for a browser admin", async () => {
    stubBrowserWindow();
    h.primarySessionState = {
      data: { authenticated: true, scopes: ADMIN_SCOPES, auth: { policy: "remote-reachable" } },
    };
    h.hasCloudConfig = false;
    h.accessChangesQuery.data = accessSnapshot({
      pairingLinks: [
        pairingLink({ id: "pl-live", label: "Living room iPad" }),
        pairingLink({
          id: "pl-read-only",
          label: "Read-only tablet",
          scopes: [AuthOrchestrationReadScope],
        }),
        pairingLink({ id: "pl-anonymous" }),
        pairingLink({ id: "pl-expired", expiresAt: PAST }),
      ],
      clientSessions: [
        clientSession({
          sessionId: "session-current",
          current: true,
          connected: true,
          label: "This browser",
          os: "Windows",
          browser: "Chrome",
          ipAddress: "10.0.0.9",
        }),
        clientSession({
          sessionId: "session-live",
          connected: true,
          os: "macOS",
          browser: "Safari",
        }),
        clientSession({
          sessionId: "session-idle",
          connected: false,
          deviceType: "unknown",
          lastConnectedAt: null,
        }),
      ],
    });

    const onAccessRevoked = vi.fn();
    const markup = render(<ShareTab onAccessRevoked={onAccessRevoked} />);

    expect(markup).toContain("Paired clients");
    expect(markup).toContain("Living room iPad");
    expect(markup).toContain("1 scope");
    expect(markup).toContain("Pairing link");
    expect(markup).not.toContain("credential-pl-expired");
    expect(markup).toContain("This device");
    expect(markup).toContain("macOS · Safari");

    // Copy the shareable pairing URL (current-origin fallback: no endpoints).
    invoke(control("button", "Copy pairing URL for: URL"), "onClick");
    expect(h.copies[0]?.value).toContain("https://app.example.com/pair");
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Pairing URL copied" }),
    );

    // Secure-context URL failures distinguish full URLs from raw codes.
    h.toastAdd.mockClear();
    h.copyBehavior = "error";
    invoke(control("button", "Copy pairing URL for: URL"), "onClick");
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not copy pairing URL" }),
    );
    h.copyBehavior = "copy";

    // Copy the raw pairing code from the grouped menu.
    h.toastAdd.mockClear();
    invoke(control("menu-item", "Copy code"), "onClick");
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Pairing code copied" }),
    );

    // Copy errors open the reveal dialog and surface an error toast.
    h.toastAdd.mockClear();
    h.copyBehavior = "error";
    invoke(control("menu-item", "Copy code"), "onClick");
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not copy pairing code" }),
    );
    h.copyBehavior = "copy";

    // The reveal textarea selects its content on focus/click.
    const selectSpy = vi.fn();
    const textarea = h.controls.find(
      (entry) => entry.kind === "textarea" && typeof entry.props.onFocus === "function",
    );
    invoke(textarea!, "onFocus", { currentTarget: { select: selectSpy } });
    invoke(textarea!, "onClick", { currentTarget: { select: selectSpy } });
    expect(selectSpy).toHaveBeenCalledTimes(2);

    // Revoke a pairing link (success then failure).
    invoke(findControls("button", "Revoke")[0]!, "onClick");
    await flush();
    expect(h.revokeServerPairingLink).toHaveBeenCalled();
    h.toastAdd.mockClear();
    h.revokeServerPairingLink.mockRejectedValueOnce(new Error("revoke failed"));
    invoke(findControls("button", "Revoke")[0]!, "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not revoke pairing link" }),
    );

    // Revoke a client session (success then failure).
    invoke(findControls("button", "Revoke").at(-1)!, "onClick");
    await flush();
    expect(h.revokeServerClientSession).toHaveBeenCalled();
    expect(onAccessRevoked).toHaveBeenCalledTimes(2);
    h.toastAdd.mockClear();
    h.revokeServerClientSession.mockRejectedValueOnce(new Error("session revoke failed"));
    invoke(findControls("button", "Revoke").at(-1)!, "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not revoke client access" }),
    );

    // Revoke all other clients (success then failure).
    h.toastAdd.mockClear();
    invoke(control("button", "Revoke others"), "onClick");
    await flush();
    expect(h.revokeOtherServerClientSessions).toHaveBeenCalled();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Revoked 1 other client" }),
    );
    h.toastAdd.mockClear();
    h.revokeOtherServerClientSessions.mockResolvedValueOnce(3);
    invoke(control("button", "Revoke others"), "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Revoked 3 clients" }),
    );
    h.toastAdd.mockClear();
    h.revokeOtherServerClientSessions.mockRejectedValueOnce(new Error("bulk revoke failed"));
    invoke(control("button", "Revoke others"), "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not revoke other clients" }),
    );

    for (const dialog of findControls("dialog", "false")) {
      invoke(dialog, "onOpenChange", false);
      invoke(dialog, "onOpenChange", true);
    }
    invoke(control("button", "Done"), "onClick");
  });

  it("falls back to reveal dialogs when the clipboard is unavailable on a loopback host", () => {
    stubBrowserWindow({ hostname: "localhost", secure: false, clipboard: false });
    h.hasCloudConfig = false;
    h.primarySessionState = {
      data: { authenticated: true, scopes: ADMIN_SCOPES, auth: { policy: "remote-reachable" } },
    };
    h.accessChangesQuery.data = accessSnapshot({
      pairingLinks: [pairingLink({ id: "pl-local" })],
      clientSessions: [],
    });

    const markup = render();

    // Loopback host + no endpoints: no shareable URL, token-only guidance.
    expect(markup).toContain("Show code");
    expect(markup).toContain("Copy the token and pair from another client");
    expect(markup).toContain("Clipboard copy is unavailable here.");
  });

  it("keeps paired-client management available for local-only browser admins", () => {
    stubBrowserWindow();
    h.hasCloudConfig = false;
    h.primarySessionState = {
      data: { authenticated: true, scopes: ADMIN_SCOPES, auth: { policy: "local-only" } },
    };

    const markup = render();

    expect(markup).toContain("Paired clients");
    expect(markup).not.toContain("Enable network access");
  });

  it("renders the full desktop backend surface and drives its handlers", async () => {
    const bridge = stubDesktopWindow();
    h.primaryEnvironment = {
      environmentId: PRIMARY_ID,
      serverConfig: { mismatch: { clientVersion: "1.2.3", serverVersion: "1.2.4" } },
    };
    h.environments = [
      environment({
        id: "environment-wsl",
        label: "WSL Backend",
        targetTag: "DesktopLocalConnectionTarget",
        connection: { phase: "connected" },
      }),
    ];
    h.networkAccessQuery.data = {
      serverExposureState: {
        mode: "network-accessible",
        endpointUrl: "https://desktop.example.com",
        advertisedHost: "desktop.local",
        tailscaleServeEnabled: false,
        tailscaleServePort: 443,
      },
      advertisedEndpoints: [
        endpoint({
          id: "desktop-loopback:1",
          label: "Loopback",
          httpBaseUrl: "http://127.0.0.1:5133",
          reachability: "loopback",
        }),
        endpoint({
          id: "desktop-lan:1",
          label: "LAN",
          httpBaseUrl: "http://192.168.1.20:5133",
          isDefault: true,
        }),
        endpoint({
          id: "tailscale-ip:1",
          label: "Tailscale IP",
          httpBaseUrl: "https://100.100.1.2:443",
          hostedHttpsApp: "compatible",
          reachability: "private-network",
        }),
        endpoint({
          id: "custom:1",
          label: "Odd Custom",
          httpBaseUrl: "gopher://custom.example.com",
          providerId: "manual",
        }),
        endpoint({
          id: "custom:2",
          label: "Unavailable Custom",
          httpBaseUrl: "http://unavailable.example.com",
          status: "unavailable",
          providerId: "manual",
        }),
        endpoint({
          id: "tailscale-magicdns:1",
          label: "Tailscale HTTPS",
          httpBaseUrl: "https://machine.tailnet.ts.net",
          status: "unavailable",
          reachability: "private-network",
          hostedHttpsApp: "compatible",
        }),
      ],
    };
    h.accessChangesQuery.data = accessSnapshot({
      pairingLinks: [pairingLink({ id: "pl-desktop", label: "Phone" })],
      clientSessions: [clientSession({ sessionId: "session-desktop", connected: true })],
    });
    h.uiState.defaultAdvertisedEndpointKey = "desktop-core:lan:http";

    const markup = render();

    expect(markup).toContain("Version drift");
    expect(markup).toContain("http://192.168.1.20:5133");
    expect(markup).toContain("Tailscale HTTPS");
    expect(markup).toContain("BiBCode Connect");
    expect(markup).toContain("Paired clients");

    // The pairing link resolves URLs against the advertised endpoints.
    expect(markup).toContain("Copy pairing URL for: LAN");
    invoke(control("button", "Copy pairing URL for: LAN"), "onClick");
    expect(h.copies[0]?.value).toContain("http://192.168.1.20:5133");

    // Endpoint copy menu contains backend URLs and hosted app links.
    invoke(control("menu-item", "Tailscale IP"), "onClick");
    expect(h.copies[1]?.value).toContain("/pair?host=");
    invoke(control("menu-item", "LAN"), "onClick");
    invoke(control("menu-item", "Loopback"), "onClick");
    invoke(control("menu-item", "Odd Custom"), "onClick");

    h.toastAdd.mockClear();
    h.copyBehavior = "error";
    invoke(control("menu-item", "Tailscale IP"), "onClick");
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not copy hosted app link" }),
    );
    h.copyBehavior = "copy";

    // Only the Tailscale confirmation owns a restart control here.
    for (const entry of findControls("button", "Restart and disable")) {
      invoke(entry, "onClick");
    }
    await flush();
    expect(bridge.setTailscaleServeEnabled).toHaveBeenCalledTimes(1);
    expect(bridge.setTailscaleServeEnabled).toHaveBeenCalledWith({ enabled: false, port: 443 });
    expect(bridge.applyServerExposure).not.toHaveBeenCalled();
    expect(h.refreshDesktopNetworkAccessState).toHaveBeenCalled();

    // Disabling tailscale can fail with a toast.
    h.toastAdd.mockClear();
    bridge.setTailscaleServeEnabled.mockRejectedValueOnce(new Error("serve down"));
    const disableButtons = findControls("button", "Restart and disable");
    invoke(disableButtons.at(-1)!, "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not disable Tailscale HTTPS" }),
    );

    // Tailscale setup dialog confirm (valid default port).
    bridge.setTailscaleServeEnabled.mockClear();
    invoke(control("button", "Enable"), "onClick");
    await flush();
    expect(bridge.setTailscaleServeEnabled).toHaveBeenCalledWith({ enabled: true, port: 443 });
    h.toastAdd.mockClear();
    bridge.setTailscaleServeEnabled.mockRejectedValueOnce(new Error("setup failed"));
    invoke(control("button", "Enable"), "onClick");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not set up Tailscale HTTPS" }),
    );

    // The tailscale switch stages setup/disable flows.
    invoke(control("switch", "Enable Tailscale HTTPS"), "onCheckedChange", true);
    invoke(control("switch", "Enable Tailscale HTTPS"), "onCheckedChange", false);

    // Tailscale port input.
    invoke(control("input", "number"), "onChange", { target: { value: "8443" } });

    // Dialog open-change plumbing.
    for (const dialog of findControls("alert-dialog", "false")) {
      invoke(dialog, "onOpenChange", false);
      if (typeof dialog.props.onOpenChangeComplete === "function") {
        invoke(dialog, "onOpenChangeComplete", false);
        invoke(dialog, "onOpenChangeComplete", true);
      }
    }
    for (const dialog of findControls("dialog", "false")) {
      invoke(dialog, "onOpenChange", false);
    }

    // Add-environment dialog (manual fields live under Advanced).
    const hostInput = control("input", "backend.example.com");
    invoke(hostInput, "onChange", {
      target: { value: "https://pairhost.example.com/pair?token=abc123" },
    });
    invoke(hostInput, "onChange", { target: { value: "plain-host.example.com" } });
    invoke(hostInput, "onChange", { target: { value: "   " } });
    invoke(hostInput, "onChange", {
      target: {
        value: "https://hosted.example.com/pair?host=https%3A%2F%2Fbackend.example.com&token=tok9",
      },
    });
    invoke(control("input", "PAIRCODE"), "onChange", { target: { value: "CODE99" } });

    // Adding with empty fields fails fast with a toast.
    h.toastAdd.mockClear();
    clickButton("Add manually");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Could not add backend",
        description: "Enter a backend host.",
      }),
    );
    expect(h.commands.connectPairing).not.toHaveBeenCalled();

    // Cloud link row renders because the desktop session is administrative.
    expect(control("switch", "Enable BiBCode Connect")).toBeDefined();
  });

  it("does not expose a manual desktop network-access control", () => {
    const bridge = stubDesktopWindow();
    h.networkAccessQuery.data = {
      serverExposureState: {
        mode: "local-only",
        endpointUrl: null,
        advertisedHost: null,
        tailscaleServeEnabled: false,
        tailscaleServePort: 443,
      },
      advertisedEndpoints: [],
    };
    const markup = render();
    expect(markup).not.toContain("Enable network access");
    expect(markup).not.toContain("Enable network access?");
    expect(bridge.applyServerExposure).not.toHaveBeenCalled();
  });

  it("renders busy states with the endpoint rail expanded and SSH mode active", async () => {
    stubDesktopWindow();
    h.stateOverrides.set(false, true);
    h.stateOverrides.set("pairing-code", "ssh");
    h.stateOverrides.set("443", "0");
    h.networkAccessQuery.data = {
      serverExposureState: {
        mode: "network-accessible",
        endpointUrl: "https://desktop.example.com",
        advertisedHost: null,
        tailscaleServeEnabled: true,
        tailscaleServePort: 443,
      },
      advertisedEndpoints: [
        endpoint({
          id: "desktop-lan:1",
          label: "LAN",
          httpBaseUrl: "http://192.168.1.20:5133",
        }),
        endpoint({
          id: "desktop-loopback:1",
          label: "Loopback",
          httpBaseUrl: "http://127.0.0.1:5133",
          reachability: "loopback",
        }),
        endpoint({
          id: "tailscale-magicdns:1",
          label: "Tailscale HTTPS",
          httpBaseUrl: "https://machine.tailnet.ts.net",
          status: "available",
          hostedHttpsApp: "compatible",
        }),
      ],
    };
    h.wslQuery.data = {
      enabled: true,
      distro: "Ubuntu",
      available: true,
      wslOnly: true,
      distros: [{ name: "Ubuntu", isDefault: true, version: 2 }],
      preflightError: null,
    } satisfies DesktopWslState;
    h.accessChangesQuery.data = accessSnapshot({
      pairingLinks: [pairingLink({ id: "pl-busy" })],
      clientSessions: [clientSession({ sessionId: "session-busy", connected: true })],
    });
    h.sshHostsQuery.data = [
      {
        alias: "devbox",
        hostname: "devbox.internal",
        username: "dev",
        port: 22,
        source: "ssh-config",
      },
      {
        alias: "spare",
        hostname: "spare.internal",
        username: null,
        port: null,
        source: "known-hosts",
      },
    ] satisfies ReadonlyArray<DesktopDiscoveredSshHost>;

    const markup = render();

    // Busy affordances driven by the overridden boolean state.
    expect(markup).toContain("Restarting…");
    expect(markup).toContain("Adding…");
    expect(markup).toContain("Revoking…");

    // The endpoint rail is expanded and renders per-endpoint actions.
    expect(markup).toContain("LAN");
    expect(markup).toContain("Loopback");
    invoke(control("button", "Set as default"), "onClick");
    expect(h.uiState.setDefaultAdvertisedEndpointKey).toHaveBeenCalled();

    // While tailscale updates are running the rail and confirm buttons all
    // render their busy labels; invoking them exercises the busy guards.
    expect(markup).toContain("Enter a port from 1 to 65535.");
    for (const entry of findControls("button", "Restarting…")) {
      if (typeof entry.props.onClick === "function") {
        invoke(entry, "onClick");
      }
    }
    for (const entry of findControls("button", "Applying…")) {
      if (typeof entry.props.onClick === "function") {
        invoke(entry, "onClick");
      }
    }
    await flush();

    // Busy dialogs refuse to close while their operation is running.
    for (const dialog of h.controls.filter(
      (entry) => entry.kind === "dialog" || entry.kind === "alert-dialog",
    )) {
      invoke(dialog, "onOpenChange", false);
    }

    // SSH mode renders discovered hosts.
    expect(markup).toContain("Suggested hosts");
    expect(markup).toContain("devbox");
    expect(markup).toContain("spare");
    invoke(findControls("button", "Refresh")[0]!, "onClick");
    expect(h.sshHostsQuery.refresh).toHaveBeenCalled();

    // Connect one of the discovered hosts.
    clickButton("Add environment");
    await flush();
    expect(h.commands.connectSsh).toHaveBeenCalled();
  });

  it("adds a remote backend through the pairing form", async () => {
    stubDesktopWindow();
    h.stateOverrides.set("", "pairhost.example.com");
    h.wslQuery.data = null;

    render();

    clickButton("Add manually");
    await flush();
    expect(h.commands.connectPairing).toHaveBeenCalledWith({
      host: "pairhost.example.com",
      pairingCode: "pairhost.example.com",
    });
    expect(h.toastAdd).toHaveBeenCalledWith(expect.objectContaining({ title: "Backend added" }));

    // Failure path.
    h.toastAdd.mockClear();
    h.commands.connectPairing.mockResolvedValueOnce(failure(new Error("pairing rejected")));
    clickButton("Add manually");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not add backend", description: "pairing rejected" }),
    );

    // Interrupted commands stay silent.
    h.toastAdd.mockClear();
    h.commands.connectPairing.mockResolvedValueOnce(failure(new Error("interrupted"), true));
    clickButton("Add manually");
    await flush();
    expect(h.toastAdd).not.toHaveBeenCalled();
  });

  // "Updated" means the host answered with an environment that is already
  // saved; the alias is not the environment (two users on one host differ).
  const savedSshEnvironment = () =>
    environment({
      id: "environment-ssh",
      label: "Build box",
      targetTag: "SshConnectionTarget",
      sshTarget: { alias: "10.0.0.5", hostname: "10.0.0.5", username: "alice", port: 22 },
    });

  it("says a manual re-add of a saved SSH environment updated it", async () => {
    stubDesktopWindow();
    h.stateOverrides.set("pairing-code", "ssh");
    h.stateOverrides.set("", "10.0.0.5:2222");
    h.wslQuery.data = null;
    h.sshHostsQuery.data = [];
    h.environments = [
      environment({
        id: "environment-ssh",
        label: "Build box",
        targetTag: "SshConnectionTarget",
        sshTarget: { alias: "build-box", hostname: "10.0.0.5", username: null, port: 22 },
      }),
    ];
    h.commands.connectSsh.mockResolvedValueOnce(success(EnvironmentId.make("environment-ssh")));

    render();

    clickButton("Add environment");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Environment updated",
        description: "10.0.0.5 is connecting over SSH.",
      }),
    );
  });

  it("says a different user on a saved SSH host was added", async () => {
    stubDesktopWindow();
    h.stateOverrides.set("pairing-code", "ssh");
    h.stateOverrides.set("", "10.0.0.5:2222");
    h.wslQuery.data = null;
    h.sshHostsQuery.data = [];
    h.environments = [savedSshEnvironment()];
    h.commands.connectSsh.mockResolvedValueOnce(success(EnvironmentId.make("environment-ssh-bob")));

    render();

    clickButton("Add environment");
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Environment added" }),
    );
  });

  for (const [returned, title] of [
    ["environment-ssh", "Environment updated"],
    ["environment-ssh-new", "Environment added"],
  ] as const) {
    it(`uses the returned environment for a discovered host (${title})`, async () => {
      stubDesktopWindow();
      // Opens the add dialog, which lists discovered hosts.
      h.stateOverrides.set(false, true);
      h.stateOverrides.set("pairing-code", "ssh");
      h.wslQuery.data = null;
      h.sshHostsQuery.data = [
        {
          alias: "devbox",
          hostname: "devbox.internal",
          username: "dev",
          port: 22,
          source: "ssh-config",
        },
      ];
      h.environments = [savedSshEnvironment()];
      h.commands.connectSsh.mockResolvedValueOnce(success(EnvironmentId.make(returned)));

      render();

      const row = findControls("button", "Add environment").find(
        (entry) => typeof entry.props.onClick === "function",
      );
      invoke(row!, "onClick");
      await flush();
      expect(h.commands.connectSsh).toHaveBeenCalledWith(
        expect.objectContaining({ target: expect.objectContaining({ alias: "devbox" }) }),
      );
      expect(h.toastAdd).toHaveBeenCalledWith(expect.objectContaining({ title }));
    });
  }

  it("adds SSH backends with parsed manual targets", async () => {
    stubDesktopWindow();
    h.stateOverrides.set("pairing-code", "ssh");
    h.stateOverrides.set("", "10.0.0.5:2222");
    h.wslQuery.data = null;
    h.sshHostsQuery.data = [];

    render();

    clickButton("Add environment");
    await flush();
    expect(h.commands.connectSsh).toHaveBeenCalledTimes(1);
    const target = (
      h.commands.connectSsh.mock.calls[0]![0] as {
        target: { hostname: string; username: string | null; port: number | null };
      }
    ).target;
    expect(target.hostname).toBe("10.0.0.5");
    expect(target.username).toBe("10.0.0.5:2222");
    expect(target.port).toBe(10);
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({
        title: "Environment added",
        description: "10.0.0.5 is connecting over SSH.",
      }),
    );

    // SSH connect failures strip IPC prefixes from the error text.
    h.toastAdd.mockClear();
    h.commands.connectSsh.mockResolvedValueOnce(
      failure(
        new Error(
          "Error invoking remote method 'desktop:ensure-ssh-environment': SshConnectionError: host unreachable",
        ),
      ),
    );
    clickButton("Add environment");
    await flush();
    expect(h.toastAdd).not.toHaveBeenCalled();
  });

  it("rejects manual SSH targets with invalid ports", async () => {
    stubDesktopWindow();
    h.stateOverrides.set("pairing-code", "ssh");
    h.stateOverrides.set("", "user@wsl-box");
    h.wslQuery.data = null;
    h.sshHostsQuery.data = [];

    render();

    clickButton("Add environment");
    await flush();
    expect(h.commands.connectSsh).not.toHaveBeenCalled();
  });

  it("parses bracketed IPv6 SSH hosts", async () => {
    stubDesktopWindow();
    h.stateOverrides.set("pairing-code", "ssh");
    h.stateOverrides.set("", "[2001:db8::1]:8443");
    h.wslQuery.data = null;
    h.sshHostsQuery.data = [];

    render();

    // The port field mirrors the host text and fails to parse as a number.
    clickButton("Add environment");
    await flush();
    expect(h.commands.connectSsh).not.toHaveBeenCalled();
  });

  it("manages the BiBCode Connect link", async () => {
    stubBrowserWindow();
    h.primarySessionState = {
      data: { authenticated: true, scopes: ADMIN_SCOPES, auth: { policy: "remote-reachable" } },
    };
    h.hasCloudConfig = true;

    const markup = render();
    expect(markup).toContain("This environment is available to your other devices");

    const linkSwitch = control("switch", "Enable BiBCode Connect");

    // Successful link.
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    expect(h.commands.link).toHaveBeenCalledWith({
      target: h.cloudLinkState.target,
      clerkToken: "clerk-token",
    });
    expect(h.cloudLinkState.refresh).toHaveBeenCalled();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "BiBCode Connect linked" }),
    );

    // Successful unlink.
    h.toastAdd.mockClear();
    invoke(linkSwitch, "onCheckedChange", false);
    await flush();
    expect(h.commands.unlink).toHaveBeenCalledWith({
      target: h.cloudLinkState.target,
      clerkToken: "clerk-token",
    });
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "BiBCode Connect unlinked" }),
    );

    // Clerk token failures report an update failure with a trace id action.
    h.toastAdd.mockClear();
    const tokenError = Object.assign(new Error("token expired"), { traceId: "trace-42" });
    h.clerkAuth.getToken.mockRejectedValueOnce(tokenError);
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    const failureToast = h.toastAdd.mock.calls[0]?.[0] as {
      title: string;
      data?: { secondaryActionProps?: { onClick: () => void } };
    };
    expect(failureToast.title).toBe("Could not update BiBCode Connect");
    failureToast.data?.secondaryActionProps?.onClick();

    // Missing token while linking asks the user to sign in.
    h.toastAdd.mockClear();
    h.clerkAuth.getToken.mockResolvedValueOnce(null);
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not update BiBCode Connect" }),
    );

    // Link command failure.
    h.toastAdd.mockClear();
    h.commands.link.mockResolvedValueOnce(failure(new Error("link denied")));
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not update BiBCode Connect" }),
    );

    // Interrupted link command stays silent.
    h.toastAdd.mockClear();
    h.commands.link.mockResolvedValueOnce(failure(new Error("interrupted"), true));
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    expect(h.toastAdd).not.toHaveBeenCalled();

    // Relay refresh failure after a successful link.
    h.toastAdd.mockClear();
    h.commands.relayRefresh.mockResolvedValueOnce(failure(new Error("refresh failed")));
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not update BiBCode Connect" }),
    );

    // Interrupted refreshes stay silent, and unlinking permits a missing Clerk token.
    h.toastAdd.mockClear();
    h.commands.relayRefresh.mockResolvedValueOnce(failure(new Error("interrupted"), true));
    invoke(linkSwitch, "onCheckedChange", true);
    await flush();
    expect(h.toastAdd).not.toHaveBeenCalled();

    h.clerkAuth.getToken.mockResolvedValueOnce(null);
    invoke(linkSwitch, "onCheckedChange", false);
    await flush();
    expect(h.commands.unlink).toHaveBeenLastCalledWith({
      target: h.cloudLinkState.target,
      clerkToken: null,
    });
  });

  it("reports a missing local environment when toggling BiBCode Connect too early", async () => {
    stubBrowserWindow();
    h.primarySessionState = {
      data: { authenticated: true, scopes: ADMIN_SCOPES, auth: { policy: "local-only" } },
    };
    h.cloudLinkState = {
      target: null,
      data: null,
      error: "cloud state failed",
      isPending: true,
      refresh: vi.fn(),
    };

    const markup = render();
    expect(markup).toContain("cloud state failed");
    expect(markup).toContain("Make this environment available");

    invoke(control("switch", "Enable BiBCode Connect"), "onCheckedChange", true);
    await flush();
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Could not update BiBCode Connect" }),
    );
    expect(h.commands.link).not.toHaveBeenCalled();
  });

  it("shows sign-in guidance when the clerk session is missing", () => {
    stubBrowserWindow();
    h.clerkAuth.isSignedIn = false;
    h.primarySessionState = {
      data: { authenticated: true, scopes: ADMIN_SCOPES, auth: { policy: "local-only" } },
    };
    h.cloudLinkState.data = { linked: false };

    const markup = render();
    expect(markup).toContain("Sign in to BiBCode Connect to manage this environment.");
  });

  it("lists connectable BiBCode Connect environments with availability states", async () => {
    stubBrowserWindow();
    h.primarySessionState = { data: null };
    h.hasCloudConfig = true;
    h.relayDiscovery = {
      refreshing: false,
      environments: new Map([
        [
          "environment-online",
          relayEnvironmentEntry({
            id: "environment-online",
            label: "Online Env",
            availability: "online",
          }),
        ],
        [
          "environment-offline",
          relayEnvironmentEntry({
            id: "environment-offline",
            label: "Offline Env",
            availability: "offline",
          }),
        ],
        [
          "environment-checking",
          relayEnvironmentEntry({
            id: "environment-checking",
            label: "Checking Env",
            availability: "checking",
          }),
        ],
        [
          "environment-error",
          relayEnvironmentEntry({
            id: "environment-error",
            label: "Error Env",
            availability: "error",
            errorMessage: "relay probe failed",
          }),
        ],
        [
          "environment-primary",
          relayEnvironmentEntry({
            id: "environment-primary",
            label: "Primary",
            availability: "online",
          }),
        ],
      ]),
    };

    const markup = render();

    expect(markup).toContain("Online Env");
    expect(markup).toContain("Available · Relay online");
    expect(markup).toContain("Available · Relay offline");
    expect(markup).toContain("Available · Checking relay status…");
    expect(markup).toContain("relay probe failed");
    // The primary environment is filtered out of the connectable list.
    expect(markup).not.toContain(">Primary<");

    // Connect an environment successfully.
    invoke(findControls("button", "Connect")[0]!, "onClick");
    await flush();
    expect(h.commands.register).toHaveBeenCalledTimes(1);
    expect(h.commands.register).toHaveBeenCalledWith({
      _tag: "RelayConnectionRegistration",
      target: {
        _tag: "RelayConnectionTarget",
        environmentId: EnvironmentId.make("environment-online"),
        label: "Online Env",
      },
    });
    expect(h.toastAdd).toHaveBeenCalledWith(
      expect.objectContaining({ title: "Environment connected" }),
    );

    // Failure with a trace id.
    h.toastAdd.mockClear();
    h.commands.register.mockResolvedValueOnce(
      failure(Object.assign(new Error("register failed"), { traceId: "trace-7" })),
    );
    invoke(findControls("button", "Connect")[0]!, "onClick");
    await flush();
    const errorToast = h.toastAdd.mock.calls[0]?.[0] as {
      title: string;
      data?: { secondaryActionProps?: { onClick: () => void } };
    };
    expect(errorToast.title).toBe("Could not connect environment");
    errorToast.data?.secondaryActionProps?.onClick();

    // Interrupted registration stays silent.
    h.toastAdd.mockClear();
    h.commands.register.mockResolvedValueOnce(failure(new Error("interrupted"), true));
    invoke(findControls("button", "Connect")[0]!, "onClick");
    await flush();
    expect(h.toastAdd).not.toHaveBeenCalled();
  });

  it("renders saved-backend boundary states and management restrictions", () => {
    stubBrowserWindow();
    h.hasCloudConfig = true;
    h.primarySessionState = {
      data: {
        authenticated: true,
        scopes: [...STANDARD_SCOPES, AuthAccessReadScope, AuthAccessWriteScope],
        auth: { policy: "remote-reachable" },
      },
    };
    h.environments = [
      environment({
        id: "environment-connecting",
        label: "Connecting backend",
        connection: { phase: "connecting" },
      }),
      environment({
        id: "environment-reconnecting",
        label: "Reconnecting backend",
        connection: { phase: "reconnecting" },
      }),
      environment({
        id: "environment-error-no-trace",
        label: "Errored backend",
        connection: { phase: "error", error: new Error("transport failed"), traceId: null },
      }),
      environment({
        id: "environment-ssh-minimal",
        label: "Minimal SSH",
        targetTag: "SshConnectionTarget",
        sshTarget: {
          alias: "build-host",
          hostname: "build-host.internal",
          username: null,
          port: null,
        },
      }),
      environment({
        id: "environment-relay-idle",
        label: "Relay managed",
        relayManaged: true,
      }),
    ];

    const markup = render();

    // Both in-progress rows label their action "Connecting…", and the connecting row's
    // status copy now uses the same ellipsis character.
    expect(findControls("button", "Connecting…")).toHaveLength(2);
    expect(markup.match(/Connecting…/gu)).toHaveLength(5);
    expect(markup).not.toContain("Connecting...");
    expect(markup).toContain("Connection failed. Reason: Error: transport failed");
    expect(markup).not.toContain("Copy trace ID");
    expect(markup).toContain("SSH build-host.internal");
    expect(markup).toContain("BiBCode Connect");
    expect(control("switch", "Enable BiBCode Connect").props.disabled).toBe(true);
  });

  it("renders every available network endpoint row", () => {
    installMountedDesktopWindow();
    const exposureState = {
      mode: "network-accessible",
      endpointUrl: "https://desktop.example.com" as string | null,
      advertisedHost: null as string | null,
      tailscaleServeEnabled: false,
      tailscaleServePort: 443,
    };
    h.networkAccessQuery.data = {
      serverExposureState: exposureState,
      advertisedEndpoints: [
        endpoint({
          id: "desktop-lan:primary",
          label: "Primary LAN",
          httpBaseUrl: "http://192.168.1.20:5133",
          isDefault: true,
        }),
        endpoint({
          id: "desktop-lan:secondary",
          label: "Secondary LAN",
          httpBaseUrl: "http://192.168.1.21:5133",
        }),
      ],
    };
    const markup = render();
    expect(markup).toContain("http://192.168.1.20:5133");
    expect(markup).toContain("http://192.168.1.21:5133");
  });

  it("shows a skeleton while the first relay refresh is in flight", () => {
    stubBrowserWindow();
    h.primarySessionState = { data: null };
    h.hasCloudConfig = true;
    h.relayDiscovery = { refreshing: true, environments: new Map() };

    const markup = render();
    expect(markup).toContain("data-skeleton");
  });

  it("shows the cloud-enabled empty state when no environments exist anywhere", () => {
    stubBrowserWindow();
    h.primarySessionState = { data: null };
    h.hasCloudConfig = true;
    h.relayDiscovery = { refreshing: false, environments: new Map() };

    const markup = render();
    expect(markup).toContain("No saved remote environments");
    expect(markup).toContain("connect one from BiBCode Connect");
  });

  it("shows version, compatibility, and transport metadata for a saved server", () => {
    stubBrowserWindow();
    const environmentId = EnvironmentId.make("env-presentation");
    h.compatVerdicts.set(environmentId, { kind: "legacy" });
    h.environments = [
      {
        environmentId,
        label: "AI-SERVER",
        relayManaged: false,
        serverConfig: {
          environment: {
            serverVersion: "1.4.2",
            capabilities: { remoteUpdateControl: false },
          },
        },
        connection: { phase: "connected", error: null, traceId: null },
        entry: {
          target: { _tag: "BearerConnectionTarget", connectionId: "bearer:env-presentation" },
          profile: Option.some({ _tag: "BearerConnectionProfile", hostKey: null }),
        },
      },
    ];

    const markup = render();
    expect(markup).toContain("AI-SERVER");
    expect(markup).toContain("BiBCode v1.4.2");
    expect(markup).toContain("Limited compatibility");
    expect(markup).toContain("Unencrypted");
  });

  it("shows Status unavailable after a failed probe without a cached descriptor", () => {
    stubBrowserWindow();
    const environmentId = EnvironmentId.make("env-unavailable");
    h.environments = [
      {
        environmentId,
        label: "LAB",
        relayManaged: false,
        serverConfig: null,
        connection: { phase: "error", error: "connect ECONNREFUSED", traceId: null },
        entry: {
          target: { _tag: "BearerConnectionTarget", connectionId: "bearer:env-unavailable" },
          profile: Option.none(),
        },
      },
    ];

    const markup = render();
    expect(markup).toContain("Status unavailable");
    expect(markup).toContain("Connection failed. Reason: connect ECONNREFUSED");
  });
});
