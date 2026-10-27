import type { AuthShareStateResult, DesktopServerExposureState } from "@coddy/contracts";
import * as DateTime from "effect/DateTime";
import { RefreshCwIcon } from "lucide-react";
import {
  type ReactElement,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import { readCurrentEnvironmentPresentationPolicy } from "~/connection/currentEnvironmentPresentation";
import {
  cancelServerPairingOffer,
  createServerPairingOffer,
  getServerShareState,
  PRIMARY_PAIRING_OFFER_REQUEST_TIMEOUT_MS,
  usePrimarySessionState,
} from "~/environments/primary";
import {
  desktopNetworkAccessStateAtom,
  refreshDesktopNetworkAccessState,
} from "~/state/desktopNetworkAccess";
import { desktopWslStateAtom } from "~/state/desktopWslState";
import {
  useEnvironmentHttpBaseUrl,
  usePrimaryEnvironment,
  usePrimaryEnvironmentId,
} from "~/state/environments";
import { useEnvironmentQuery } from "~/state/query";
import {
  reconcileShareExposureOnce,
  withShareExposureBridgeTimeout,
} from "~/state/shareExposureReconciler";
import { randomUUID } from "~/lib/utils";

import { Button } from "../../ui/button";
import { Input } from "../../ui/input";
import { QRCodeSvg } from "../../ui/qr-code";
import { Radio, RadioGroup } from "../../ui/radio-group";
import { SettingsRow, SettingsSection } from "../settingsLayout";
import { shareClassForPairingEndpoint } from "./endpointClass";
import {
  type GeneratedShareOffer,
  type GenerateShareOfferFailure,
  generateShareOffer,
  nativeShareAddressUnavailableReason,
  resolveShareAddressOptions,
  type ShareIntent,
} from "./shareOffer";
import { ShareTab } from "./ShareTab";

const FATAL_MINT_CAUSE_TAGS = new Set([
  "EnvironmentRequestInvalidError",
  "EnvironmentScopeRequiredError",
  "EnvironmentAuthInvalidError",
  "EnvironmentOperationForbiddenError",
]);

function classifyMintError(error: unknown): "retryable" | "fatal" {
  const cause =
    typeof error === "object" && error !== null && "cause" in error
      ? (error as { cause?: { _tag?: string } }).cause
      : undefined;
  return cause?._tag !== undefined && FATAL_MINT_CAUSE_TAGS.has(cause._tag) ? "fatal" : "retryable";
}

function defaultOfferName(label: string | null): string {
  const trimmed = label?.trim();
  return trimmed ? trimmed : "Coddy Studio Server";
}

function copyText(value: string): void {
  void navigator.clipboard?.writeText(value);
}

export function canResumeLegacyExposure(
  shareState: AuthShareStateResult,
  exposureState: DesktopServerExposureState,
): boolean {
  return (
    exposureState.management === "native" &&
    exposureState.mode === "local-only" &&
    exposureState.configuredMode === "network-accessible" &&
    shareState.desiredExposure === "loopback" &&
    shareState.offHostGrantCount === 0 &&
    shareState.legacyGrantCount > 0
  );
}

export function ShareThisHostTab(): ReactElement {
  const desktopBridge = window.desktopBridge;
  const presentationPolicy = readCurrentEnvironmentPresentationPolicy();
  const hasDesktopBridge = presentationPolicy.surface === "desktop" && desktopBridge !== undefined;
  const primaryEnvironment = usePrimaryEnvironment();
  const primarySessionState = usePrimarySessionState();
  const primaryEnvironmentId = usePrimaryEnvironmentId();
  const primaryHttpBaseUrl = useEnvironmentHttpBaseUrl(primaryEnvironmentId);
  const desktopNetworkAccess = useEnvironmentQuery(
    hasDesktopBridge ? desktopNetworkAccessStateAtom : null,
  );
  const desktopWsl = useEnvironmentQuery(hasDesktopBridge ? desktopWslStateAtom : null);
  const wslOnlyPrimary = desktopWsl.data?.wslOnly === true;
  const canManageNativeExposure = hasDesktopBridge && desktopWsl.data?.wslOnly === false;
  const canManageNativeExposureRef = useRef(canManageNativeExposure);
  useLayoutEffect(() => {
    canManageNativeExposureRef.current = canManageNativeExposure;
  }, [canManageNativeExposure]);
  const exposureState = desktopNetworkAccess.data?.serverExposureState ?? null;
  const advertisedEndpoints = desktopNetworkAccess.data?.advertisedEndpoints ?? [];
  const [intent, setIntent] = useState<ShareIntent>("another-device");
  const [offerName, setOfferName] = useState("");
  const [customAddress, setCustomAddress] = useState("");
  const [selectedOptionId, setSelectedOptionId] = useState<string | null>(null);
  const [offer, setOffer] = useState<GeneratedShareOffer | null>(null);
  const [failure, setFailure] = useState<GenerateShareOfferFailure | null>(null);
  const [isGenerating, setIsGenerating] = useState(false);
  const [shareState, setShareState] = useState<AuthShareStateResult | null>(null);
  const [isResumingLegacy, setIsResumingLegacy] = useState(false);
  const [legacyResumeError, setLegacyResumeError] = useState<string | null>(null);

  const refreshShareState = useCallback(
    (): Promise<void> =>
      getServerShareState().then(setShareState, (error: unknown) => {
        console.warn("[remote-sharing] Could not refresh server share state.", error);
      }),
    [],
  );

  useEffect(() => {
    void refreshShareState();
  }, [refreshShareState]);

  const options = useMemo(
    () =>
      resolveShareAddressOptions({
        intent,
        advertisedEndpoints,
        exposureState,
        primaryHttpBaseUrl,
      }),
    [advertisedEndpoints, exposureState, intent, primaryHttpBaseUrl],
  );
  const selectedOption =
    options.find((option) => option.id === selectedOptionId) ??
    (wslOnlyPrimary
      ? options.find(
          (option) => option.httpBaseUrl !== null && option.requiresExplicitSelection !== true,
        )
      : options.find((option) => option.requiresExplicitSelection !== true)) ??
    null;
  const wslExposureEndpoint = wslOnlyPrimary
    ? (advertisedEndpoints.find(
        (endpoint) =>
          endpoint.status === "available" &&
          shareClassForPairingEndpoint(endpoint.httpBaseUrl) === "off-host",
      )?.httpBaseUrl ?? null)
    : null;
  const effectiveName = defaultOfferName(
    offerName === "" ? (primaryEnvironment?.serverConfig?.environment.label ?? null) : offerName,
  );
  const willWiden =
    selectedOption !== null &&
    canManageNativeExposure &&
    exposureState?.mode !== "network-accessible" &&
    intent === "another-device";

  const handleRefresh = useCallback(() => {
    refreshDesktopNetworkAccessState();
    void refreshShareState();
  }, [refreshShareState]);

  const handleResumeLegacy = useCallback(async () => {
    if (desktopBridge === undefined) return;
    setIsResumingLegacy(true);
    setLegacyResumeError(null);
    try {
      await desktopBridge.applyServerExposure("network-accessible");
      refreshDesktopNetworkAccessState();
      await refreshShareState();
    } catch (error) {
      setLegacyResumeError(error instanceof Error ? error.message : String(error));
    } finally {
      setIsResumingLegacy(false);
    }
  }, [desktopBridge, refreshShareState]);

  const canResumeLegacy =
    shareState !== null &&
    exposureState !== null &&
    canResumeLegacyExposure(shareState, exposureState);

  const handleGenerate = useCallback(async () => {
    if (selectedOption === null) return;
    setIsGenerating(true);
    setFailure(null);
    setOffer(null);
    const result = await generateShareOffer({
      intent,
      name: effectiveName,
      customAddress: intent === "custom" ? customAddress : null,
      selectedOption,
      hasDesktopBridge: canManageNativeExposure,
      exposureState,
      applyServerExposure:
        !canManageNativeExposure || desktopBridge === undefined
          ? null
          : (desired) => desktopBridge.applyServerExposure(desired),
      mintOffer: async ({ idempotencyKey, ...input }) => {
        const minted = await createServerPairingOffer(input, idempotencyKey);
        return {
          code: minted.code,
          endpoint: minted.endpoint,
          name: minted.name,
          expiresAt: DateTime.formatIso(minted.expiresAt),
        };
      },
      newIdempotencyKey: randomUUID,
      classifyMintError,
      cancelOffer: cancelServerPairingOffer,
      cleanupExposureAfterFailedMint:
        !canManageNativeExposure || desktopBridge === undefined
          ? null
          : async () => {
              const outcome = await reconcileShareExposureOnce({
                getShareState: getServerShareState,
                getExposureState: () => desktopBridge.getServerExposureState(),
                applyExposure: (desired) => desktopBridge.applyServerExposure(desired),
                canStartExposure: () => canManageNativeExposureRef.current,
                operationTimeoutMs: PRIMARY_PAIRING_OFFER_REQUEST_TIMEOUT_MS,
              });
              if (outcome === "narrowed") return "local-confirmed";
              if (outcome === "widened" || outcome === "rewidened") return "active-reason";

              const [confirmedShareState, confirmedExposureState] = await Promise.all([
                getServerShareState(),
                withShareExposureBridgeTimeout(
                  "Server exposure state",
                  desktopBridge.getServerExposureState(),
                  PRIMARY_PAIRING_OFFER_REQUEST_TIMEOUT_MS,
                ),
              ]);
              if (
                confirmedExposureState.mode === "local-only" &&
                confirmedShareState.desiredExposure === "loopback"
              ) {
                return "local-confirmed";
              }
              if (
                confirmedExposureState.mode === "network-accessible" &&
                (confirmedShareState.desiredExposure === "wide" ||
                  confirmedShareState.legacyGrantCount > 0)
              ) {
                return "active-reason";
              }
              throw new Error("Remote-access cleanup could not confirm the server's exposure.");
            },
      sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      requestTimeoutMs: PRIMARY_PAIRING_OFFER_REQUEST_TIMEOUT_MS,
    });
    if (result.ok) setOffer(result.offer);
    else setFailure(result.failure);
    refreshDesktopNetworkAccessState();
    await refreshShareState();
    setIsGenerating(false);
  }, [
    customAddress,
    canManageNativeExposure,
    desktopBridge,
    effectiveName,
    exposureState,
    intent,
    refreshShareState,
    selectedOption,
  ]);

  const customAddressError = failure?.kind === "invalid-address" ? failure.message : null;
  const sectionError =
    failure === null || failure.kind === "invalid-address"
      ? null
      : failure.kind !== "mint-failed"
        ? failure.message
        : failure.cleanup === "local-confirmed"
          ? "The offer was not created. Remote access is confirmed local-only."
          : failure.cleanup === "active-reason"
            ? "The offer was not created. Remote access remains enabled because another live access reason still requires it."
            : failure.cleanup === "cancellation-unconfirmed"
              ? "The offer result could not be canceled or confirmed. Remote access was deliberately left unchanged because a live credential may exist."
              : "The offer was canceled, but remote-access cleanup could not be verified. Review Exposure and retry cleanup.";

  return (
    <>
      <SettingsSection title="Offer generator">
        <SettingsRow
          title="Name"
          description="Shown to the person pairing this server."
          control={
            <Input
              aria-label="Server name"
              value={offerName}
              placeholder={effectiveName}
              onChange={(event) => setOfferName(event.target.value)}
            />
          }
        />
        <div className="px-4 py-3">
          <RadioGroup
            aria-label="Pairing reach"
            value={intent}
            onValueChange={(value) => {
              if (value === "another-device" || value === "this-computer" || value === "custom") {
                setIntent(value);
                setSelectedOptionId(null);
                setFailure(null);
              }
            }}
          >
            <IntentOption
              value="another-device"
              title="Another device"
              description="Recommended. Uses a network address other devices can reach."
            />
            <IntentOption
              value="this-computer"
              title="This computer only"
              description="Creates a loopback offer. Other devices need a tunnel — for example SSH port forwarding — to use it."
            />
            <IntentOption
              value="custom"
              title="Custom address"
              description="For SSH tunnels, reverse proxies, or a hostname you manage."
            />
          </RadioGroup>
        </div>
        {intent === "custom" ? (
          <SettingsRow
            title="Custom address"
            description={
              customAddressError ??
              "Enter an externally managed http(s) address. Coddy Studio does not change the native listener or firewall."
            }
            status={
              customAddressError ? (
                <span className="text-destructive">{customAddressError}</span>
              ) : null
            }
            control={
              <Input
                aria-label="Custom address"
                value={customAddress}
                placeholder="https://server.example.com"
                onChange={(event) => setCustomAddress(event.target.value)}
              />
            }
          />
        ) : intent === "another-device" ? (
          <>
            <SettingsRow
              title="Address"
              description={selectedOption?.description}
              control={
                <div className="flex items-center gap-2">
                  <select
                    aria-label="Share address"
                    className="min-h-8 rounded-md border border-input bg-background px-2 text-sm"
                    value={selectedOption?.id ?? ""}
                    onChange={(event) => setSelectedOptionId(event.target.value)}
                  >
                    {options.map((option) => (
                      <option key={option.id} value={option.id}>
                        {option.label}
                      </option>
                    ))}
                  </select>
                  <Button
                    size="xs"
                    variant="outline"
                    onClick={handleRefresh}
                    aria-label="Refresh addresses"
                  >
                    <RefreshCwIcon className="size-3.5" />
                    Refresh
                  </Button>
                </div>
              }
            />
            {options.length === 0 &&
            canManageNativeExposure &&
            exposureState?.mode === "local-only" ? (
              <p className="mx-4 mb-3 text-xs text-warning">
                {nativeShareAddressUnavailableReason(advertisedEndpoints) ===
                "no-private-default-route"
                  ? "Native sharing uses the private address of this computer's default network route, and this computer has none right now. Connect it to a local network that provides a default route, then Refresh. Otherwise, use an externally managed server or reverse proxy."
                  : "Native sharing needs a private network address. For a public-only host, use an externally managed server or reverse proxy."}
              </p>
            ) : null}
          </>
        ) : null}
        <div className="mx-4 my-3 rounded-md border border-warning/35 bg-warning/10 px-3 py-2 text-xs text-foreground">
          Pairing grants your user account on this machine. A paired client can read and write
          files, run terminals, and use git as you.
        </div>
        {willWiden ? (
          <p className="mx-4 mb-3 text-xs text-warning">
            Enabling remote access restarts the local server. Running turns on this machine will
            stop.
          </p>
        ) : null}
        {sectionError ? <p className="mx-4 mb-3 text-xs text-destructive">{sectionError}</p> : null}
        <div className="px-4 pb-4">
          <Button
            disabled={isGenerating || selectedOption === null}
            onClick={() => void handleGenerate()}
          >
            {isGenerating ? "Generating…" : "Generate pairing offer"}
          </Button>
        </div>
        {offer ? <GeneratedOfferPanel offer={offer} /> : null}
      </SettingsSection>
      <SettingsSection title="Exposure">
        <SettingsRow
          title="Remote access"
          description={
            <span className="space-y-1">
              <span className="block">
                {wslOnlyPrimary
                  ? wslExposureEndpoint
                    ? `Reachable at ${wslExposureEndpoint}`
                    : "The WSL network address is unavailable."
                  : !hasDesktopBridge
                    ? primarySessionState.data?.auth.policy === "remote-reachable"
                      ? "This server is configured for remote access."
                      : "This server is limited to this machine."
                    : exposureState?.mode === "network-accessible"
                      ? exposureState.endpointUrl
                        ? `Reachable at ${exposureState.endpointUrl}`
                        : "Reachable from the network."
                      : exposureState
                        ? "Limited to this machine."
                        : "Loading…"}
              </span>
              {wslOnlyPrimary ? (
                <span className="block">
                  This WSL listener is externally managed. WSL/Hyper-V firewall policy controls
                  external reachability, and Coddy Studio cannot switch it off automatically.
                </span>
              ) : !hasDesktopBridge ? (
                <span className="block">
                  Exposure is controlled where the server is launched — restart `coddy-studio serve` with
                  `--host` to change it.
                </span>
              ) : (
                <span className="block">
                  Managed automatically for Another device pairings only: switches on while one
                  requires native exposure and back off when the last one is revoked. Custom
                  addresses remain externally managed.
                </span>
              )}
              {exposureState?.mode === "network-accessible" &&
              (shareState?.legacyGrantCount ?? 0) > 0 ? (
                <span className="block text-warning">
                  Paired clients from an earlier version keep remote access on. Revoke or re-pair
                  them to allow automatic switch-off.
                </span>
              ) : null}
              {canResumeLegacy ? (
                <span className="block space-y-2 pt-1">
                  <span className="block text-warning">
                    This host was previously shared with clients whose reach is unknown. Resume only
                    if you still trust those clients, or re-pair them for automatic exposure.
                  </span>
                  <Button
                    disabled={isResumingLegacy}
                    size="xs"
                    variant="outline"
                    onClick={() => void handleResumeLegacy()}
                  >
                    {isResumingLegacy ? "Resuming…" : "Resume legacy remote access"}
                  </Button>
                </span>
              ) : null}
              {legacyResumeError ? (
                <span className="block text-destructive">{legacyResumeError}</span>
              ) : null}
            </span>
          }
        />
      </SettingsSection>
      <ShareTab onAccessRevoked={() => void refreshShareState()} />
    </>
  );
}

function IntentOption({
  value,
  title,
  description,
}: {
  readonly value: ShareIntent;
  readonly title: string;
  readonly description: string;
}): ReactElement {
  return (
    <label className="flex cursor-pointer items-start gap-3">
      <Radio value={value} aria-label={title} />
      <span>
        <span className="block text-sm font-medium text-foreground">{title}</span>
        <span className="block text-xs text-muted-foreground">{description}</span>
      </span>
    </label>
  );
}

function GeneratedOfferPanel({ offer }: { readonly offer: GeneratedShareOffer }): ReactElement {
  return (
    <div className="mx-4 mb-4 grid gap-4 rounded-lg border border-border bg-muted/20 p-4 sm:grid-cols-[1fr_auto]">
      <div className="min-w-0 space-y-3">
        <OfferValue label="Pairing code" value={offer.code} />
        <OfferValue label="Coddy Studio link" value={offer.deepLink} />
        <OfferValue label="Open in browser — for networks you trust" value={offer.browserUrl} />
        <p className="text-xs text-warning">
          This link and the page it loads travel over plain HTTP. Prefer the pairing code for
          Coddy Studio clients.
        </p>
        {offer.reach === "this-computer" || offer.endpointClass === "loopback" ? (
          <p className="text-xs text-muted-foreground">
            Loopback offer: reachable only through a tunnel into this machine.
          </p>
        ) : null}
      </div>
      <QRCodeSvg
        value={offer.deepLink}
        size={128}
        level="M"
        marginSize={2}
        title="Pairing code — scan with a Coddy Studio client"
      />
    </div>
  );
}

function OfferValue({
  label,
  value,
}: {
  readonly label: string;
  readonly value: string;
}): ReactElement {
  return (
    <div>
      <p className="text-xs font-medium text-muted-foreground">{label}</p>
      <div className="mt-1 flex items-center gap-2">
        <code className="min-w-0 flex-1 truncate text-sm text-foreground">{value}</code>
        <Button size="xs" variant="outline" onClick={() => copyText(value)}>
          Copy
        </Button>
      </div>
    </div>
  );
}
