import { ChevronDownIcon, QrCodeIcon, TriangleAlertIcon } from "lucide-react";
import { useAuth } from "@clerk/react";
import { memo, useCallback, useMemo, useState } from "react";
import {
  AuthAccessWriteScope,
  AuthAdministrativeScopes,
  AuthRelayWriteScope,
  type AuthClientSession,
  type AuthPairingLink,
  type AdvertisedEndpoint,
} from "@bibcode/contracts";
import { findErrorTraceId } from "@bibcode/client-runtime/errors";
import {
  isAtomCommandInterrupted,
  settlePromise,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";

import { useCopyToClipboard } from "../../../hooks/useCopyToClipboard";
import { formatElapsedDurationLabel, formatExpiresInLabel } from "../../../timestampFormat";
import { resolveRelayClerkTokenOptions } from "../../../cloud/publicConfig";
import { SettingsRow, SettingsSection, useRelativeTimeTick } from "../settingsLayout";
import { Input } from "../../ui/input";
import {
  Dialog,
  DialogClose,
  DialogFooter,
  DialogDescription,
  DialogHeader,
  DialogPanel,
  DialogPopup,
  DialogTitle,
  DialogTrigger,
} from "../../ui/dialog";
import { ScrollArea } from "../../ui/scroll-area";
import {
  AlertDialog,
  AlertDialogClose,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogPopup,
  AlertDialogTitle,
} from "../../ui/alert-dialog";
import { QRCodeSvg } from "../../ui/qr-code";
import { Popover, PopoverPopup, PopoverTrigger } from "../../ui/popover";
import { Spinner } from "../../ui/spinner";
import { Switch } from "../../ui/switch";
import { stackedThreadToast, toastManager } from "../../ui/toast";
import { Tooltip, TooltipPopup, TooltipTrigger } from "../../ui/tooltip";
import { Button } from "../../ui/button";
import { Group, GroupSeparator } from "../../ui/group";
import {
  Menu,
  MenuGroup,
  MenuGroupLabel,
  MenuItem,
  MenuPopup,
  MenuSeparator,
  MenuTrigger,
} from "../../ui/menu";
import { Textarea } from "../../ui/textarea";
import {
  revokeOtherServerClientSessions,
  revokeServerClientSession,
  revokeServerPairingLink,
  isLoopbackHostname,
  usePrimarySessionState,
  type ServerClientSessionRecord,
  type ServerPairingLinkRecord,
} from "~/environments/primary";
import { useUiStateStore } from "~/uiStateStore";
import { resolveServerConfigVersionMismatch } from "~/versionSkew";
import { usePrimaryCloudLinkState } from "~/cloud/primaryCloudLinkState";
import { hasCloudPublicConfig } from "~/cloud/publicConfig";
import {
  linkPrimaryEnvironment as linkPrimaryEnvironmentAtom,
  unlinkPrimaryEnvironment as unlinkPrimaryEnvironmentAtom,
} from "~/cloud/linkEnvironmentAtoms";
import { authEnvironment } from "~/state/auth";
import { useEnvironmentQuery } from "~/state/query";
import {
  desktopNetworkAccessStateAtom,
  refreshDesktopNetworkAccessState,
} from "~/state/desktopNetworkAccess";
import { usePrimaryEnvironment } from "~/state/environments";
import { relayEnvironmentDiscovery } from "~/state/relay";
import { useAtomCommand } from "../../../state/use-atom-command";
import { resolveDesktopPairingUrl, resolveHostedPairingUrl } from "./pairingUrls";
import {
  AccessScopeSummary,
  type AccessSectionPresentation,
  accessRowClassName,
  ConnectionStatusDot,
  DEFAULT_TAILSCALE_SERVE_PORT,
  EMPTY_ADVERTISED_ENDPOINTS,
  endpointDefaultPreferenceKey,
  endpointRowClassName,
  formatAccessTimestamp,
  isHostedAppPairingUrl,
  isTailscaleHttpsEndpoint,
  ITEM_ROW_INNER_CLASSNAME,
  resolveAdvertisedEndpointPairingUrl,
  resolveCurrentOriginPairingUrl,
  selectPairingEndpoint,
  sortDesktopClientSessions,
  sortDesktopPairingLinks,
  toDesktopClientSessionRecord,
  toDesktopPairingLinkRecord,
} from "./shared";

type PairingLinkListRowProps = {
  pairingLink: ServerPairingLinkRecord;
  endpointUrl: string | null | undefined;
  endpoints: ReadonlyArray<AdvertisedEndpoint>;
  defaultEndpointKey: string | null;
  presentation?: AccessSectionPresentation;
  revokingPairingLinkId: string | null;
  onRevoke: (id: string) => void;
};

const PairingLinkListRow = memo(function PairingLinkListRow({
  pairingLink,
  endpointUrl,
  endpoints,
  defaultEndpointKey,
  presentation = "current",
  revokingPairingLinkId,
  onRevoke,
}: PairingLinkListRowProps) {
  const nowMs = useRelativeTimeTick(1_000);
  const expiresAtMs = useMemo(
    () => new Date(pairingLink.expiresAt).getTime(),
    [pairingLink.expiresAt],
  );
  const [isRevealDialogOpen, setIsRevealDialogOpen] = useState(false);

  const currentOriginPairingUrl = useMemo(
    () => resolveCurrentOriginPairingUrl(pairingLink.credential),
    [pairingLink.credential],
  );
  const hostedPairingUrl = useMemo(
    () =>
      endpointUrl != null && endpointUrl !== ""
        ? resolveHostedPairingUrl(endpointUrl, pairingLink.credential)
        : null,
    [endpointUrl, pairingLink.credential],
  );
  const endpointPairingUrl = useMemo(() => {
    const endpoint = selectPairingEndpoint(endpoints, defaultEndpointKey);
    return endpoint ? resolveAdvertisedEndpointPairingUrl(endpoint, pairingLink.credential) : null;
  }, [defaultEndpointKey, endpoints, pairingLink.credential]);
  const endpointCopyOptions = useMemo(() => {
    const options: Array<{
      readonly key: string;
      readonly label: string;
      readonly url: string;
      readonly detail: string;
    }> = [];
    for (const endpoint of endpoints) {
      if (endpoint.status === "unavailable") {
        continue;
      }
      const url = resolveAdvertisedEndpointPairingUrl(endpoint, pairingLink.credential);
      options.push({
        key: endpointDefaultPreferenceKey(endpoint),
        label: endpoint.label,
        url,
        detail: isHostedAppPairingUrl(url) ? "Hosted app link" : "Backend pairing URL",
      });
    }
    return options;
  }, [endpoints, pairingLink.credential]);
  const shareablePairingUrl =
    endpointPairingUrl ??
    (endpointUrl != null && endpointUrl !== ""
      ? (hostedPairingUrl ?? resolveDesktopPairingUrl(endpointUrl, pairingLink.credential))
      : isLoopbackHostname(window.location.hostname)
        ? null
        : currentOriginPairingUrl);
  const revealValue = shareablePairingUrl ?? pairingLink.credential;
  const isShareableHostedAppPairingUrl =
    shareablePairingUrl !== null && isHostedAppPairingUrl(shareablePairingUrl);
  const canCopyToClipboard =
    typeof window !== "undefined" &&
    window.isSecureContext &&
    navigator.clipboard?.writeText != null;

  const { copyToClipboard } = useCopyToClipboard<"code" | "hosted-link" | "link">({
    onCopy: (kind) => {
      toastManager.add({
        type: "success",
        title:
          kind === "hosted-link"
            ? "Hosted app link copied"
            : kind === "link"
              ? "Pairing URL copied"
              : "Pairing code copied",
        description:
          kind === "hosted-link"
            ? "Open it in the browser on the device you want to connect."
            : kind === "link"
              ? "Open it in the client you want to pair to this environment."
              : "Paste it into another client to finish pairing.",
      });
    },
    onError: (error, kind) => {
      setIsRevealDialogOpen(true);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: canCopyToClipboard
            ? kind === "hosted-link"
              ? "Could not copy hosted app link"
              : kind === "link"
                ? "Could not copy pairing URL"
                : "Could not copy pairing code"
            : "Clipboard copy unavailable",
          description: canCopyToClipboard ? error.message : "Showing the full value instead.",
        }),
      );
    },
  });

  const copyPairingValue = useCallback(
    (value: string, kind: "code" | "hosted-link" | "link") => {
      copyToClipboard(value, kind);
    },
    [copyToClipboard],
  );

  const copyKindForUrl = useCallback(
    (url: string): "hosted-link" | "link" => (isHostedAppPairingUrl(url) ? "hosted-link" : "link"),
    [],
  );

  const handleCopyCode = useCallback(() => {
    copyPairingValue(pairingLink.credential, "code");
  }, [copyPairingValue, pairingLink.credential]);

  const handleCopyDefaultLink = useCallback(() => {
    if (!shareablePairingUrl) return;
    copyPairingValue(shareablePairingUrl, copyKindForUrl(shareablePairingUrl));
  }, [copyKindForUrl, copyPairingValue, shareablePairingUrl]);

  const expiresAbsolute = formatAccessTimestamp(pairingLink.expiresAt);

  const primaryLabel = pairingLink.label ?? "Pairing link";
  const defaultEndpointCopyOption =
    endpointCopyOptions.find((option) => option.key === defaultEndpointKey) ??
    endpointCopyOptions[0] ??
    null;
  const defaultEndpointCopyLabel = defaultEndpointCopyOption?.label ?? "URL";
  const backendEndpointCopyOptions = endpointCopyOptions.filter(
    (option) => !isHostedAppPairingUrl(option.url),
  );
  const hostedEndpointCopyOptions = endpointCopyOptions.filter((option) =>
    isHostedAppPairingUrl(option.url),
  );
  const renderEndpointMenuItems = (
    options: typeof endpointCopyOptions = endpointCopyOptions,
    renderDetail = true,
  ) =>
    options.map((option) => (
      <MenuItem
        key={option.key}
        onClick={() => copyPairingValue(option.url, copyKindForUrl(option.url))}
      >
        <span className="min-w-0 flex-1">
          <span className="block truncate">{option.label}</span>
          {renderDetail ? (
            <span className="block truncate text-xs text-muted-foreground">{option.detail}</span>
          ) : null}
        </span>
      </MenuItem>
    ));
  const renderPairingCodeMenuItem = (renderDetail = true) => (
    <MenuItem onClick={handleCopyCode}>
      <span className="min-w-0 flex-1">
        <span className="block truncate">Copy code</span>
        {renderDetail ? (
          <span className="block truncate text-xs text-muted-foreground">Token only</span>
        ) : null}
      </span>
    </MenuItem>
  );
  const renderCompactEndpointGroup = (
    label: string,
    options: typeof endpointCopyOptions,
    includeSeparator: boolean,
  ) =>
    options.length > 0 ? (
      <>
        {includeSeparator ? <MenuSeparator /> : null}
        <MenuGroup>
          <MenuGroupLabel>{label}</MenuGroupLabel>
          {renderEndpointMenuItems(options, false)}
        </MenuGroup>
      </>
    ) : null;
  const renderGroupedCopyMenuItems = (options?: { codeFirst?: boolean }) => (
    <>
      {options?.codeFirst ? (
        <>
          <MenuGroup>
            <MenuGroupLabel>Pairing code</MenuGroupLabel>
            {renderPairingCodeMenuItem(false)}
          </MenuGroup>
          {endpointCopyOptions.length > 0 ? <MenuSeparator /> : null}
        </>
      ) : null}
      {renderCompactEndpointGroup("Pairing URLs", backendEndpointCopyOptions, false)}
      {renderCompactEndpointGroup(
        "Hosted app link",
        hostedEndpointCopyOptions,
        backendEndpointCopyOptions.length > 0,
      )}
      {!options?.codeFirst ? (
        <>
          {endpointCopyOptions.length > 0 ? <MenuSeparator /> : null}
          <MenuGroup>
            <MenuGroupLabel>Pairing code</MenuGroupLabel>
            {renderPairingCodeMenuItem(false)}
          </MenuGroup>
        </>
      ) : null}
    </>
  );

  if (expiresAtMs <= nowMs) {
    return null;
  }

  return (
    <div className={accessRowClassName(presentation)}>
      <div className={ITEM_ROW_INNER_CLASSNAME}>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex min-h-5 items-center gap-1.5">
            <ConnectionStatusDot
              tooltipText={`Link created at ${formatAccessTimestamp(pairingLink.createdAt)}`}
              dotClassName="bg-amber-400"
            />
            <h3 className="text-sm font-medium text-foreground">{primaryLabel}</h3>
            <Popover>
              {shareablePairingUrl ? (
                <>
                  <PopoverTrigger
                    openOnHover
                    delay={250}
                    closeDelay={100}
                    render={
                      <button
                        type="button"
                        className="inline-flex size-4 shrink-0 items-center justify-center rounded-sm text-muted-foreground/50 outline-none hover:text-foreground"
                        aria-label="Show QR code"
                      />
                    }
                  >
                    <QrCodeIcon aria-hidden className="size-3" />
                  </PopoverTrigger>
                  <PopoverPopup side="top" align="start" tooltipStyle className="w-max">
                    <QRCodeSvg
                      value={shareablePairingUrl}
                      size={88}
                      level="M"
                      marginSize={2}
                      title="Pairing link — scan to open on another device"
                    />
                  </PopoverPopup>
                </>
              ) : null}
            </Popover>
          </div>
          <p className="text-xs text-muted-foreground" title={expiresAbsolute}>
            {formatExpiresInLabel(pairingLink.expiresAt, nowMs)}
            <span aria-hidden> · </span>
            <AccessScopeSummary scopes={pairingLink.scopes} label="Pairing link scopes" />
          </p>
          {shareablePairingUrl === null ? (
            <p className="text-xs text-muted-foreground">
              Copy the token and pair from another client using this backend&apos;s reachable host.
            </p>
          ) : null}
        </div>
        <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto sm:justify-end">
          <Dialog open={isRevealDialogOpen} onOpenChange={setIsRevealDialogOpen}>
            {canCopyToClipboard ? (
              <>
                {shareablePairingUrl ? (
                  <Group aria-label="Copy selected endpoint">
                    <Button
                      size="xs"
                      variant="outline"
                      className="max-w-56"
                      title={`Copy pairing URL for: ${defaultEndpointCopyLabel}`}
                      onClick={handleCopyDefaultLink}
                    >
                      <span className="truncate">
                        Copy pairing URL for: {defaultEndpointCopyLabel}
                      </span>
                    </Button>
                    <GroupSeparator />
                    <Menu>
                      <MenuTrigger
                        render={
                          <Button
                            size="icon-xs"
                            variant="outline"
                            aria-label="Choose endpoint to copy"
                          />
                        }
                      >
                        <ChevronDownIcon className="size-3.5" />
                      </MenuTrigger>
                      <MenuPopup align="end" className="min-w-60">
                        {renderGroupedCopyMenuItems()}
                      </MenuPopup>
                    </Menu>
                  </Group>
                ) : (
                  <Button size="xs" variant="outline" onClick={handleCopyCode}>
                    Copy code
                  </Button>
                )}
              </>
            ) : (
              <DialogTrigger render={<Button size="xs" variant="outline" />}>
                {shareablePairingUrl ? "Show link" : "Show code"}
              </DialogTrigger>
            )}
            <DialogPopup className="max-w-md">
              <DialogHeader>
                <DialogTitle>
                  {shareablePairingUrl
                    ? isShareableHostedAppPairingUrl
                      ? "Hosted app pairing link"
                      : "Pairing link"
                    : "Pairing code"}
                </DialogTitle>
                <DialogDescription>
                  {shareablePairingUrl
                    ? isShareableHostedAppPairingUrl
                      ? "Clipboard copy is unavailable here. Open or manually copy this hosted app link on the device you want to connect."
                      : "Clipboard copy is unavailable here. Open or manually copy this full pairing URL on the device you want to connect."
                    : "Clipboard copy is unavailable here. Manually copy this code into another client."}
                </DialogDescription>
              </DialogHeader>
              <DialogPanel className="space-y-4">
                <Textarea
                  readOnly
                  value={revealValue}
                  rows={shareablePairingUrl ? 4 : 3}
                  className="text-xs leading-relaxed"
                  onFocus={(event) => event.currentTarget.select()}
                  onClick={(event) => event.currentTarget.select()}
                />
                {shareablePairingUrl ? (
                  <div className="flex justify-center rounded-xl border border-border/60 bg-muted/30 p-4">
                    <QRCodeSvg
                      value={shareablePairingUrl}
                      size={132}
                      level="M"
                      marginSize={2}
                      title="Pairing link — scan to open on another device"
                    />
                  </div>
                ) : null}
              </DialogPanel>
              <DialogFooter variant="bare">
                <Button variant="outline" onClick={() => setIsRevealDialogOpen(false)}>
                  Done
                </Button>
                {canCopyToClipboard ? (
                  <Button variant="outline" size="xs" onClick={handleCopyCode}>
                    Copy code
                  </Button>
                ) : null}
              </DialogFooter>
            </DialogPopup>
          </Dialog>
          <Button
            size="xs"
            variant="destructive-outline"
            disabled={revokingPairingLinkId === pairingLink.id}
            onClick={() => void onRevoke(pairingLink.id)}
          >
            {revokingPairingLinkId === pairingLink.id ? "Revoking…" : "Revoke"}
          </Button>
        </div>
      </div>
    </div>
  );
});

type ConnectedClientListRowProps = {
  clientSession: ServerClientSessionRecord;
  presentation?: AccessSectionPresentation;
  revokingClientSessionId: string | null;
  onRevokeSession: (sessionId: ServerClientSessionRecord["sessionId"]) => void;
};

const ConnectedClientListRow = memo(function ConnectedClientListRow({
  clientSession,
  presentation = "current",
  revokingClientSessionId,
  onRevokeSession,
}: ConnectedClientListRowProps) {
  const nowMs = useRelativeTimeTick(1_000);
  const isLive = clientSession.current || clientSession.connected;
  const lastConnectedAt = clientSession.lastConnectedAt;
  const statusTooltip = isLive
    ? lastConnectedAt
      ? `Connected for ${formatElapsedDurationLabel(lastConnectedAt, nowMs)}`
      : "Connected"
    : lastConnectedAt
      ? `Last connected at ${formatAccessTimestamp(lastConnectedAt)}`
      : "Not connected yet.";
  const deviceInfoBits = [
    clientSession.client.deviceType !== "unknown"
      ? clientSession.client.deviceType[0]?.toUpperCase() + clientSession.client.deviceType.slice(1)
      : null,
    clientSession.client.os ?? null,
    clientSession.client.browser ?? null,
    clientSession.client.ipAddress ?? null,
  ].filter((value): value is string => value !== null);
  const primaryLabel =
    clientSession.client.label ??
    ([clientSession.client.os, clientSession.client.browser].filter(Boolean).join(" · ") ||
      clientSession.subject);

  return (
    <div className={accessRowClassName(presentation)}>
      <div className={ITEM_ROW_INNER_CLASSNAME}>
        <div className="min-w-0 flex-1 space-y-1">
          <div className="flex min-h-5 items-center gap-1.5">
            <ConnectionStatusDot
              tooltipText={statusTooltip}
              dotClassName={isLive ? "bg-success" : "bg-muted-foreground/30"}
              pingClassName={isLive ? "bg-success/60 duration-2000" : null}
            />
            <h3 className="text-sm font-medium text-foreground">{primaryLabel}</h3>
            {clientSession.current ? (
              <span className="text-xs text-muted-foreground rounded-md border border-border/50 bg-muted/50 px-1 py-0.5">
                This device
              </span>
            ) : null}
          </div>
          <p className="text-xs text-muted-foreground">
            {deviceInfoBits.length > 0 ? (
              <>
                {deviceInfoBits.join(" · ")}
                <span aria-hidden> · </span>
              </>
            ) : null}
            <AccessScopeSummary scopes={clientSession.scopes} label="Client scopes" />
          </p>
        </div>
        <div className="flex w-full shrink-0 items-center gap-2 sm:w-auto sm:justify-end">
          {!clientSession.current ? (
            <Button
              size="xs"
              variant="destructive-outline"
              disabled={revokingClientSessionId === clientSession.sessionId}
              onClick={() => void onRevokeSession(clientSession.sessionId)}
            >
              {revokingClientSessionId === clientSession.sessionId ? "Revoking…" : "Revoke"}
            </Button>
          ) : null}
        </div>
      </div>
    </div>
  );
});

type AuthorizedClientsHeaderActionProps = {
  clientSessions: ReadonlyArray<ServerClientSessionRecord>;
  isRevokingOtherClients: boolean;
  onRevokeOtherClients: () => void;
};

const AuthorizedClientsHeaderAction = memo(function AuthorizedClientsHeaderAction({
  clientSessions,
  isRevokingOtherClients,
  onRevokeOtherClients,
}: AuthorizedClientsHeaderActionProps) {
  return (
    <Button
      size="xs"
      variant="destructive-outline"
      disabled={
        isRevokingOtherClients || clientSessions.every((clientSession) => clientSession.current)
      }
      onClick={() => void onRevokeOtherClients()}
    >
      {isRevokingOtherClients ? "Revoking…" : "Revoke others"}
    </Button>
  );
});

type PairingClientsListProps = {
  endpointUrl: string | null | undefined;
  endpoints: ReadonlyArray<AdvertisedEndpoint>;
  defaultEndpointKey: string | null;
  presentation?: AccessSectionPresentation;
  isLoading: boolean;
  pairingLinks: ReadonlyArray<ServerPairingLinkRecord>;
  clientSessions: ReadonlyArray<ServerClientSessionRecord>;
  revokingPairingLinkId: string | null;
  revokingClientSessionId: string | null;
  onRevokePairingLink: (id: string) => void;
  onRevokeClientSession: (sessionId: ServerClientSessionRecord["sessionId"]) => void;
};

const PairingClientsList = memo(function PairingClientsList({
  endpointUrl,
  endpoints,
  defaultEndpointKey,
  presentation = "current",
  isLoading,
  pairingLinks,
  clientSessions,
  revokingPairingLinkId,
  revokingClientSessionId,
  onRevokePairingLink,
  onRevokeClientSession,
}: PairingClientsListProps) {
  return (
    <>
      {pairingLinks.map((pairingLink) => (
        <PairingLinkListRow
          key={pairingLink.id}
          pairingLink={pairingLink}
          endpointUrl={endpointUrl}
          endpoints={endpoints}
          defaultEndpointKey={defaultEndpointKey}
          presentation={presentation}
          revokingPairingLinkId={revokingPairingLinkId}
          onRevoke={onRevokePairingLink}
        />
      ))}

      {clientSessions.map((clientSession) => (
        <ConnectedClientListRow
          key={clientSession.sessionId}
          clientSession={clientSession}
          presentation={presentation}
          revokingClientSessionId={revokingClientSessionId}
          onRevokeSession={onRevokeClientSession}
        />
      ))}

      {pairingLinks.length === 0 && clientSessions.length === 0 && !isLoading ? (
        <div className={accessRowClassName(presentation)}>
          <p className="text-xs text-muted-foreground">No pairing links or client sessions.</p>
        </div>
      ) : null}
    </>
  );
});

type AdvertisedEndpointListRowProps = {
  endpoint: AdvertisedEndpoint;
  isDefault: boolean;
  presentation?: AccessSectionPresentation;
  onSetDefault: (endpoint: AdvertisedEndpoint) => void;
  onSetupTailscaleServe: (endpoint: AdvertisedEndpoint) => void;
  onDisableTailscaleServe: (endpoint: AdvertisedEndpoint) => void;
  isUpdatingTailscaleServe: boolean;
};

const AdvertisedEndpointListRow = memo(function AdvertisedEndpointListRow({
  endpoint,
  isDefault,
  presentation = "current",
  onSetDefault,
  onSetupTailscaleServe,
  onDisableTailscaleServe,
  isUpdatingTailscaleServe,
}: AdvertisedEndpointListRowProps) {
  const isAvailable = endpoint.status === "available";
  const needsTailscaleSetup = isTailscaleHttpsEndpoint(endpoint) && endpoint.status !== "available";
  const canDisableTailscaleServe =
    isTailscaleHttpsEndpoint(endpoint) && endpoint.status === "available";
  const shouldShowEndpointUrl = !needsTailscaleSetup;
  const isEndpointRail = presentation === "endpoint-rail";
  return (
    <div className={endpointRowClassName(presentation, isAvailable)}>
      {isEndpointRail && isDefault ? (
        <span className="absolute inset-y-2 left-0 w-1 rounded-r-full bg-primary" aria-hidden />
      ) : null}
      <div className="flex min-h-6 min-w-0 flex-col gap-2 sm:-my-0.5 sm:flex-row sm:items-center">
        <div className="flex min-w-0 items-baseline gap-3">
          <h3 className="shrink-0 text-sm leading-5 font-medium text-foreground">
            {endpoint.label}
          </h3>
          {shouldShowEndpointUrl ? (
            <p
              className="min-w-0 truncate text-xs leading-5 text-muted-foreground"
              title={endpoint.httpBaseUrl}
            >
              {endpoint.httpBaseUrl}
            </p>
          ) : null}
          {!isAvailable ? (
            <span className="shrink-0 rounded-md border border-border/70 px-1 py-0.5 text-xs text-muted-foreground">
              Setup required
            </span>
          ) : null}
        </div>
        <div className="ml-auto flex min-h-6 shrink-0 items-center justify-end gap-2">
          {isDefault ? (
            <span className="rounded-md border border-primary/30 bg-primary/10 px-1 py-0.5 text-xs text-primary">
              Default
            </span>
          ) : null}
          {needsTailscaleSetup ? (
            <Button
              size="xs"
              variant="outline"
              onClick={() => onSetupTailscaleServe(endpoint)}
              disabled={isUpdatingTailscaleServe}
            >
              {isUpdatingTailscaleServe ? "Restarting…" : "Setup"}
            </Button>
          ) : null}
          {canDisableTailscaleServe ? (
            <Button
              size="xs"
              variant="destructive-outline"
              onClick={() => onDisableTailscaleServe(endpoint)}
              disabled={isUpdatingTailscaleServe}
            >
              {isUpdatingTailscaleServe ? "Restarting…" : "Disable"}
            </Button>
          ) : null}
          {!needsTailscaleSetup && !isDefault ? (
            <Button size="xs" variant="outline" onClick={() => onSetDefault(endpoint)}>
              Set as default
            </Button>
          ) : null}
        </div>
      </div>
    </div>
  );
});

function CloudLinkSwitch({
  checked,
  disabled,
  disabledReason,
  onCheckedChange,
}: {
  readonly checked: boolean;
  readonly disabled: boolean;
  readonly disabledReason: string | null;
  readonly onCheckedChange?: (enabled: boolean) => void;
}) {
  const control = (
    <Switch
      aria-label="Enable BiBCode Connect"
      checked={checked}
      disabled={disabled}
      {...(onCheckedChange ? { onCheckedChange } : {})}
    />
  );
  return disabledReason ? (
    <Tooltip>
      <TooltipTrigger render={<span className="inline-flex">{control}</span>} />
      <TooltipPopup side="top">{disabledReason}</TooltipPopup>
    </Tooltip>
  ) : (
    control
  );
}

function ConfiguredCloudLinkRow({ canManageRelay }: { readonly canManageRelay: boolean }) {
  const { getToken, isSignedIn } = useAuth();
  const refreshRelayEnvironments = useAtomCommand(relayEnvironmentDiscovery.refresh, {
    reportFailure: false,
  });
  const linkPrimaryEnvironment = useAtomCommand(linkPrimaryEnvironmentAtom, {
    reportFailure: false,
  });
  const unlinkPrimaryEnvironment = useAtomCommand(unlinkPrimaryEnvironmentAtom, {
    reportFailure: false,
  });
  const primaryCloudLinkState = usePrimaryCloudLinkState();
  const [operationError, setOperationError] = useState<string | null>(null);
  const [isUpdating, setIsUpdating] = useState(false);

  const reportUpdateFailure = (cause: unknown) => {
    const message =
      cause instanceof Error ? cause.message : "Could not update BiBCode Connect access.";
    const traceId = findErrorTraceId(cause);
    console.error("[bibcode-connect] Could not update BiBCode Connect", {
      message,
      traceId,
      cause,
    });
    setOperationError(traceId ? `${message} Trace ID: ${traceId}` : message);
    toastManager.add({
      type: "error",
      title: "Could not update BiBCode Connect",
      description: message,
      data: traceId
        ? {
            secondaryActionProps: {
              children: "Copy trace ID",
              onClick: () => void navigator.clipboard?.writeText(traceId),
            },
          }
        : undefined,
    });
  };

  const updateLink = async (enabled: boolean) => {
    setIsUpdating(true);
    setOperationError(null);
    const tokenResult = await settlePromise(() => getToken(resolveRelayClerkTokenOptions()));
    if (tokenResult._tag === "Failure") {
      reportUpdateFailure(squashAtomCommandFailure(tokenResult));
      setIsUpdating(false);
      return;
    }

    const target = primaryCloudLinkState.target;
    if (!target) {
      reportUpdateFailure(new Error("Local environment is not ready yet."));
      setIsUpdating(false);
      return;
    }
    if (enabled && !tokenResult.value) {
      reportUpdateFailure(new Error("Sign in to BiBCode Connect before linking this environment."));
      setIsUpdating(false);
      return;
    }

    const linkResult =
      enabled && tokenResult.value
        ? await linkPrimaryEnvironment({
            target,
            clerkToken: tokenResult.value,
          })
        : await unlinkPrimaryEnvironment({
            target,
            clerkToken: tokenResult.value ?? null,
          });
    if (linkResult._tag === "Failure") {
      if (!isAtomCommandInterrupted(linkResult)) {
        reportUpdateFailure(squashAtomCommandFailure(linkResult));
      }
      setIsUpdating(false);
      return;
    }

    primaryCloudLinkState.refresh();
    const refreshResult = await refreshRelayEnvironments();
    if (refreshResult._tag === "Failure") {
      if (!isAtomCommandInterrupted(refreshResult)) {
        reportUpdateFailure(squashAtomCommandFailure(refreshResult));
      }
      setIsUpdating(false);
      return;
    }

    toastManager.add({
      type: "success",
      title: enabled ? "BiBCode Connect linked" : "BiBCode Connect unlinked",
      description: enabled
        ? "This environment is available through BiBCode Connect."
        : "This environment is no longer available through BiBCode Connect.",
    });
    setIsUpdating(false);
  };

  const disabledReason = !isSignedIn
    ? "Sign in to BiBCode Connect to manage this environment."
    : !canManageRelay
      ? "Your session does not have permission to manage BiBCode Connect access."
      : null;
  const linked = primaryCloudLinkState.data?.linked ?? false;

  return (
    <>
      <SettingsRow
        title="BiBCode Connect"
        description={
          linked
            ? "This environment is available to your other devices through BiBCode Connect."
            : "Make this environment available to your other devices through BiBCode Connect."
        }
        status={operationError ?? primaryCloudLinkState.error}
        control={
          <CloudLinkSwitch
            checked={linked}
            disabled={
              !canManageRelay || !isSignedIn || primaryCloudLinkState.isPending || isUpdating
            }
            disabledReason={disabledReason}
            onCheckedChange={(enabled) => void updateLink(enabled)}
          />
        }
      />
    </>
  );
}

function CloudLinkRow({ canManageRelay }: { readonly canManageRelay: boolean }) {
  return hasCloudPublicConfig() ? <ConfiguredCloudLinkRow canManageRelay={canManageRelay} /> : null;
}

export function ShareTab({
  onAccessRevoked,
}: {
  readonly onAccessRevoked?: () => void;
} = {}) {
  const desktopBridge = window.desktopBridge;
  const primaryEnvironment = usePrimaryEnvironment();
  const primaryEnvironmentId = primaryEnvironment?.environmentId ?? null;
  const primarySessionState = usePrimarySessionState();
  const currentSessionScopes = desktopBridge
    ? AuthAdministrativeScopes
    : primarySessionState.data?.authenticated
      ? (primarySessionState.data.scopes ?? null)
      : null;
  const [desktopServerExposureMutationError, setDesktopServerExposureMutationError] = useState<
    string | null
  >(null);
  const [desktopAccessManagementMutationError, setDesktopAccessManagementMutationError] = useState<
    string | null
  >(null);
  const [revokingDesktopPairingLinkId, setRevokingDesktopPairingLinkId] = useState<string | null>(
    null,
  );
  const [revokingDesktopClientSessionId, setRevokingDesktopClientSessionId] = useState<
    string | null
  >(null);
  const [isRevokingOtherDesktopClients, setIsRevokingOtherDesktopClients] = useState(false);
  const [isUpdatingTailscaleServe, setIsUpdatingTailscaleServe] = useState(false);
  const [pendingTailscaleServeEndpoint, setPendingTailscaleServeEndpoint] =
    useState<AdvertisedEndpoint | null>(null);
  const [disableTailscaleServeDialogOpen, setDisableTailscaleServeDialogOpen] = useState(false);
  const [tailscaleServePortInput, setTailscaleServePortInput] = useState(
    String(DEFAULT_TAILSCALE_SERVE_PORT),
  );
  const primaryServerConfig = primaryEnvironment?.serverConfig ?? null;
  const primaryVersionMismatch = resolveServerConfigVersionMismatch(primaryServerConfig);
  const defaultAdvertisedEndpointKey = useUiStateStore(
    (state) => state.defaultAdvertisedEndpointKey,
  );
  const setDefaultAdvertisedEndpointKey = useUiStateStore(
    (state) => state.setDefaultAdvertisedEndpointKey,
  );
  const canManageLocalBackend = currentSessionScopes?.includes(AuthAccessWriteScope) ?? false;
  const canManageRelay = currentSessionScopes?.includes(AuthRelayWriteScope) ?? false;
  const authAccessChanges = useEnvironmentQuery(
    canManageLocalBackend && primaryEnvironmentId !== null
      ? authEnvironment.accessChanges({
          environmentId: primaryEnvironmentId,
          input: null,
        })
      : null,
  );
  const desktopNetworkAccess = useEnvironmentQuery(
    canManageLocalBackend && desktopBridge ? desktopNetworkAccessStateAtom : null,
  );
  const desktopServerExposureState = desktopNetworkAccess.data?.serverExposureState ?? null;
  const desktopAdvertisedEndpoints =
    desktopNetworkAccess.data?.advertisedEndpoints ?? EMPTY_ADVERTISED_ENDPOINTS;
  const desktopServerExposureError =
    desktopServerExposureMutationError ?? desktopNetworkAccess.error;
  const desktopAccessManagementError =
    desktopAccessManagementMutationError ?? authAccessChanges.error;
  const isLoadingDesktopAccessManagement =
    authAccessChanges.isPending && authAccessChanges.data === null;
  const desktopPairingLinks = useMemo(() => {
    const event = authAccessChanges.data;
    if (event?.type !== "snapshot") return [];
    return sortDesktopPairingLinks(
      event.payload.pairingLinks.map((pairingLink: AuthPairingLink) =>
        toDesktopPairingLinkRecord(pairingLink),
      ),
    );
  }, [authAccessChanges.data]);
  const desktopClientSessions = useMemo(() => {
    const event = authAccessChanges.data;
    if (event?.type !== "snapshot") return [];
    return sortDesktopClientSessions(
      event.payload.clientSessions.map((clientSession: AuthClientSession) =>
        toDesktopClientSessionRecord(clientSession),
      ),
    );
  }, [authAccessChanges.data]);
  const isLocalBackendNetworkAccessible = desktopServerExposureState?.mode === "network-accessible";
  const trimmedTailscaleServePortInput = tailscaleServePortInput.trim();
  const parsedTailscaleServePort = Number(trimmedTailscaleServePortInput);
  const isTailscaleServePortValid =
    /^\d+$/u.test(trimmedTailscaleServePortInput) &&
    Number.isInteger(parsedTailscaleServePort) &&
    parsedTailscaleServePort >= 1 &&
    parsedTailscaleServePort <= 65_535;

  const pendingTailscaleServeBaseUrl = useMemo(() => {
    if (!pendingTailscaleServeEndpoint) return null;
    if (!isTailscaleServePortValid) return pendingTailscaleServeEndpoint.httpBaseUrl;
    if (parsedTailscaleServePort === DEFAULT_TAILSCALE_SERVE_PORT) {
      return pendingTailscaleServeEndpoint.httpBaseUrl;
    }
    try {
      const url = new URL(pendingTailscaleServeEndpoint.httpBaseUrl);
      url.port = String(parsedTailscaleServePort);
      return url.toString().replace(/\/$/u, "");
    } catch {
      return pendingTailscaleServeEndpoint.httpBaseUrl;
    }
  }, [isTailscaleServePortValid, parsedTailscaleServePort, pendingTailscaleServeEndpoint]);

  const handleConfirmTailscaleServeSetup = useCallback(async () => {
    if (!desktopBridge) return;
    if (!isTailscaleServePortValid) return;
    setIsUpdatingTailscaleServe(true);
    setDesktopServerExposureMutationError(null);
    try {
      await desktopBridge.setTailscaleServeEnabled({
        enabled: true,
        port: parsedTailscaleServePort,
      });
      refreshDesktopNetworkAccessState();
      setPendingTailscaleServeEndpoint(null);
    } catch (error) {
      const message =
        error instanceof Error ? error.message : "Failed to configure Tailscale HTTPS.";
      setDesktopServerExposureMutationError(message);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not set up Tailscale HTTPS",
          description: message,
        }),
      );
    } finally {
      setIsUpdatingTailscaleServe(false);
    }
  }, [desktopBridge, isTailscaleServePortValid, parsedTailscaleServePort]);

  const handleStartTailscaleServeSetup = useCallback(
    (endpoint: AdvertisedEndpoint) => {
      setTailscaleServePortInput(
        String(desktopServerExposureState?.tailscaleServePort ?? DEFAULT_TAILSCALE_SERVE_PORT),
      );
      setPendingTailscaleServeEndpoint(endpoint);
    },
    [desktopServerExposureState?.tailscaleServePort],
  );

  const handleConfirmTailscaleServeDisable = useCallback(async () => {
    if (!desktopBridge) return;
    setIsUpdatingTailscaleServe(true);
    setDesktopServerExposureMutationError(null);
    try {
      await desktopBridge.setTailscaleServeEnabled({
        enabled: false,
        port: desktopServerExposureState?.tailscaleServePort ?? DEFAULT_TAILSCALE_SERVE_PORT,
      });
      refreshDesktopNetworkAccessState();
      setDisableTailscaleServeDialogOpen(false);
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to disable Tailscale HTTPS.";
      setDesktopServerExposureMutationError(message);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not disable Tailscale HTTPS",
          description: message,
        }),
      );
    } finally {
      setIsUpdatingTailscaleServe(false);
    }
  }, [desktopBridge, desktopServerExposureState?.tailscaleServePort]);

  const handleStartTailscaleServeDisable = useCallback((_endpoint: AdvertisedEndpoint) => {
    setDisableTailscaleServeDialogOpen(true);
  }, []);

  const handleRevokeDesktopPairingLink = useCallback(
    async (id: string) => {
      setRevokingDesktopPairingLinkId(id);
      setDesktopAccessManagementMutationError(null);
      try {
        await revokeServerPairingLink(id);
        onAccessRevoked?.();
      } catch (error) {
        const message = error instanceof Error ? error.message : "Failed to revoke pairing link.";
        setDesktopAccessManagementMutationError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not revoke pairing link",
            description: message,
          }),
        );
      } finally {
        setRevokingDesktopPairingLinkId(null);
      }
    },
    [onAccessRevoked],
  );

  const handleRevokeDesktopClientSession = useCallback(
    async (sessionId: ServerClientSessionRecord["sessionId"]) => {
      setRevokingDesktopClientSessionId(sessionId);
      setDesktopAccessManagementMutationError(null);
      try {
        await revokeServerClientSession(sessionId);
        onAccessRevoked?.();
      } catch (error) {
        const message = error instanceof Error ? error.message : "Failed to revoke client access.";
        setDesktopAccessManagementMutationError(message);
        toastManager.add(
          stackedThreadToast({
            type: "error",
            title: "Could not revoke client access",
            description: message,
          }),
        );
      } finally {
        setRevokingDesktopClientSessionId(null);
      }
    },
    [onAccessRevoked],
  );

  const handleRevokeOtherDesktopClients = useCallback(async () => {
    setIsRevokingOtherDesktopClients(true);
    setDesktopAccessManagementMutationError(null);
    try {
      const revokedCount = await revokeOtherServerClientSessions();
      onAccessRevoked?.();
      toastManager.add({
        type: "success",
        title: revokedCount === 1 ? "Revoked 1 other client" : `Revoked ${revokedCount} clients`,
        description: "Other paired clients will need a new pairing link before reconnecting.",
      });
    } catch (error) {
      const message = error instanceof Error ? error.message : "Failed to revoke other clients.";
      setDesktopAccessManagementMutationError(message);
      toastManager.add(
        stackedThreadToast({
          type: "error",
          title: "Could not revoke other clients",
          description: message,
        }),
      );
    } finally {
      setIsRevokingOtherDesktopClients(false);
    }
  }, [onAccessRevoked]);
  const visibleDesktopPairingLinks = desktopPairingLinks;
  const tailscaleHttpsEndpoint = useMemo(
    () => desktopAdvertisedEndpoints.find(isTailscaleHttpsEndpoint) ?? null,
    [desktopAdvertisedEndpoints],
  );
  const visibleDesktopNetworkAdvertisedEndpoints = useMemo(
    () =>
      isLocalBackendNetworkAccessible
        ? desktopAdvertisedEndpoints.filter((endpoint) => !isTailscaleHttpsEndpoint(endpoint))
        : [],
    [desktopAdvertisedEndpoints, isLocalBackendNetworkAccessible],
  );
  const visibleDesktopAdvertisedEndpoints = useMemo(
    () =>
      tailscaleHttpsEndpoint
        ? [...visibleDesktopNetworkAdvertisedEndpoints, tailscaleHttpsEndpoint]
        : visibleDesktopNetworkAdvertisedEndpoints,
    [tailscaleHttpsEndpoint, visibleDesktopNetworkAdvertisedEndpoints],
  );
  const defaultDesktopNetworkAdvertisedEndpoint = useMemo(
    () =>
      selectPairingEndpoint(visibleDesktopNetworkAdvertisedEndpoints, defaultAdvertisedEndpointKey),
    [defaultAdvertisedEndpointKey, visibleDesktopNetworkAdvertisedEndpoints],
  );
  const defaultDesktopAdvertisedEndpoint = useMemo(
    () =>
      defaultDesktopNetworkAdvertisedEndpoint ??
      selectPairingEndpoint(
        tailscaleHttpsEndpoint ? [tailscaleHttpsEndpoint] : [],
        defaultAdvertisedEndpointKey,
      ),
    [defaultAdvertisedEndpointKey, defaultDesktopNetworkAdvertisedEndpoint, tailscaleHttpsEndpoint],
  );
  const defaultDesktopAdvertisedEndpointKey = defaultDesktopAdvertisedEndpoint
    ? endpointDefaultPreferenceKey(defaultDesktopAdvertisedEndpoint)
    : null;
  const handleSetDefaultAdvertisedEndpoint = useCallback(
    (endpoint: AdvertisedEndpoint) => {
      setDefaultAdvertisedEndpointKey(endpointDefaultPreferenceKey(endpoint));
    },
    [setDefaultAdvertisedEndpointKey],
  );
  const renderEndpointRows = (presentation: AccessSectionPresentation) =>
    visibleDesktopNetworkAdvertisedEndpoints.map((endpoint) => {
      const endpointKey = endpointDefaultPreferenceKey(endpoint);
      return (
        <AdvertisedEndpointListRow
          key={endpoint.id}
          endpoint={endpoint}
          isDefault={endpointKey === defaultDesktopAdvertisedEndpointKey}
          presentation={presentation}
          onSetDefault={handleSetDefaultAdvertisedEndpoint}
          onSetupTailscaleServe={handleStartTailscaleServeSetup}
          onDisableTailscaleServe={handleStartTailscaleServeDisable}
          isUpdatingTailscaleServe={isUpdatingTailscaleServe}
        />
      );
    });
  const renderTailscaleRow = () => (
    <SettingsRow
      title="Tailscale HTTPS"
      description={
        tailscaleHttpsEndpoint
          ? tailscaleHttpsEndpoint.status === "available"
            ? tailscaleHttpsEndpoint.httpBaseUrl
            : "Use Tailscale Serve to expose this backend through a MagicDNS HTTPS URL."
          : "Start Tailscale to set up HTTPS access through MagicDNS."
      }
      status={
        desktopServerExposureError ? (
          <span className="block text-destructive">{desktopServerExposureError}</span>
        ) : null
      }
      control={
        tailscaleHttpsEndpoint ? (
          <Switch
            checked={tailscaleHttpsEndpoint.status === "available"}
            disabled={isUpdatingTailscaleServe}
            onCheckedChange={(checked) => {
              if (checked) {
                handleStartTailscaleServeSetup(tailscaleHttpsEndpoint);
                return;
              }
              handleStartTailscaleServeDisable(tailscaleHttpsEndpoint);
            }}
            aria-label="Enable Tailscale HTTPS"
          />
        ) : null
      }
    />
  );
  const renderAuthorizedClients = (presentation: AccessSectionPresentation) => (
    <>
      {desktopAccessManagementError ? (
        <div className={accessRowClassName(presentation)}>
          <p className="text-xs text-destructive">{desktopAccessManagementError}</p>
        </div>
      ) : null}
      <PairingClientsList
        endpointUrl={desktopServerExposureState?.endpointUrl}
        endpoints={visibleDesktopAdvertisedEndpoints}
        defaultEndpointKey={defaultDesktopAdvertisedEndpointKey}
        presentation={presentation}
        isLoading={isLoadingDesktopAccessManagement}
        pairingLinks={visibleDesktopPairingLinks}
        clientSessions={desktopClientSessions}
        revokingPairingLinkId={revokingDesktopPairingLinkId}
        revokingClientSessionId={revokingDesktopClientSessionId}
        onRevokePairingLink={handleRevokeDesktopPairingLink}
        onRevokeClientSession={handleRevokeDesktopClientSession}
      />
    </>
  );

  return (
    <>
      {canManageLocalBackend ? (
        <>
          <SettingsSection title="This environment">
            {primaryVersionMismatch ? (
              <SettingsRow
                title="Version drift"
                description={
                  <span className="flex items-center gap-1 text-warning">
                    <TriangleAlertIcon className="size-3.5 shrink-0" />
                    Client {primaryVersionMismatch.clientVersion}, server{" "}
                    {primaryVersionMismatch.serverVersion}. Sync them if RPC calls or reconnects
                    fail.
                  </span>
                }
              />
            ) : null}
            {desktopBridge ? (
              <>
                {renderEndpointRows("endpoint-rail")}
                {renderTailscaleRow()}
                <CloudLinkRow canManageRelay={canManageRelay} />
              </>
            ) : (
              <CloudLinkRow canManageRelay={canManageRelay} />
            )}
          </SettingsSection>

          <SettingsSection
            title="Paired clients"
            headerAction={
              <AuthorizedClientsHeaderAction
                clientSessions={desktopClientSessions}
                isRevokingOtherClients={isRevokingOtherDesktopClients}
                onRevokeOtherClients={handleRevokeOtherDesktopClients}
              />
            }
          >
            <ScrollArea
              scrollFade
              className="max-h-[22.5rem]"
              data-testid="authorized-clients-scroll-area"
            >
              {renderAuthorizedClients("current")}
            </ScrollArea>
          </SettingsSection>
          <AlertDialog
            open={disableTailscaleServeDialogOpen}
            onOpenChange={(open) => {
              if (isUpdatingTailscaleServe) return;
              setDisableTailscaleServeDialogOpen(open);
            }}
          >
            <AlertDialogPopup>
              <AlertDialogHeader>
                <AlertDialogTitle>Disable Tailscale HTTPS?</AlertDialogTitle>
                <AlertDialogDescription>
                  BiBCode will restart the local backend without Tailscale Serve.
                </AlertDialogDescription>
              </AlertDialogHeader>
              <AlertDialogFooter>
                <AlertDialogClose
                  disabled={isUpdatingTailscaleServe}
                  render={<Button variant="outline" disabled={isUpdatingTailscaleServe} />}
                >
                  Cancel
                </AlertDialogClose>
                <Button
                  variant="destructive"
                  onClick={() => void handleConfirmTailscaleServeDisable()}
                  disabled={isUpdatingTailscaleServe}
                >
                  {isUpdatingTailscaleServe ? (
                    <>
                      <Spinner className="size-3.5" />
                      Restarting…
                    </>
                  ) : (
                    "Restart and disable"
                  )}
                </Button>
              </AlertDialogFooter>
            </AlertDialogPopup>
          </AlertDialog>
          <Dialog
            open={pendingTailscaleServeEndpoint !== null}
            onOpenChange={(open) => {
              if (isUpdatingTailscaleServe) return;
              if (!open) setPendingTailscaleServeEndpoint(null);
            }}
          >
            <DialogPopup className="max-w-md">
              <DialogHeader>
                <DialogTitle>Set up Tailscale HTTPS?</DialogTitle>
                <DialogDescription>
                  BiBCode will restart the local backend with Tailscale Serve enabled and ask
                  Tailscale to proxy HTTPS traffic to this backend.
                </DialogDescription>
              </DialogHeader>
              <DialogPanel className="space-y-4">
                <label className="block">
                  <span className="text-sm font-medium text-foreground">HTTPS port</span>
                  <Input
                    className="mt-2"
                    type="number"
                    inputMode="numeric"
                    min={1}
                    max={65_535}
                    step={1}
                    value={tailscaleServePortInput}
                    onChange={(event) => setTailscaleServePortInput(event.target.value)}
                    disabled={isUpdatingTailscaleServe}
                  />
                </label>
                {!isTailscaleServePortValid ? (
                  <p className="mt-2 text-xs text-destructive">Enter a port from 1 to 65535.</p>
                ) : null}
                <div className="rounded-md border border-border/70 bg-muted/20 px-3 py-2">
                  <p className="text-xs font-medium text-muted-foreground">HTTPS endpoint</p>
                  <p
                    className="mt-1 truncate text-sm text-foreground"
                    title={pendingTailscaleServeBaseUrl ?? undefined}
                  >
                    {pendingTailscaleServeBaseUrl ?? "Pending MagicDNS endpoint"}
                  </p>
                </div>
              </DialogPanel>
              <DialogFooter>
                <DialogClose
                  disabled={isUpdatingTailscaleServe}
                  render={<Button variant="outline" disabled={isUpdatingTailscaleServe} />}
                >
                  Cancel
                </DialogClose>
                <Button
                  onClick={() => void handleConfirmTailscaleServeSetup()}
                  disabled={isUpdatingTailscaleServe || !isTailscaleServePortValid}
                >
                  {isUpdatingTailscaleServe ? (
                    <>
                      <Spinner className="size-3.5" />
                      Restarting…
                    </>
                  ) : (
                    "Enable"
                  )}
                </Button>
              </DialogFooter>
            </DialogPopup>
          </Dialog>
        </>
      ) : (
        <SettingsSection title="This environment">
          <SettingsRow
            title="Administrative access"
            description="Pairing links and client-session management require the access:write scope for this backend."
          />
          <CloudLinkRow canManageRelay={canManageRelay} />
        </SettingsSection>
      )}
    </>
  );
}
