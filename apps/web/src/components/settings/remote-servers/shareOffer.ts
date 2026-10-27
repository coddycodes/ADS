import type { AdvertisedEndpoint, DesktopServerExposureState } from "@coddy/contracts";
import { classifyPairingEndpoint, normalizeHttpBaseUrl } from "@coddy/shared/advertisedEndpoint";
import { buildBrowserPairUrl, buildPairingDeepLink } from "@coddy/shared/pairingCode";

import { raceWithDeadline } from "../../../state/shareExposureReconciler.ts";
import { shareClassForPairingEndpoint } from "./endpointClass.ts";

export type ShareIntent = "another-device" | "this-computer" | "custom";

export interface ShareAddressOption {
  readonly id: string;
  readonly label: string;
  readonly httpBaseUrl: string | null;
  readonly description?: string;
  readonly requiresExplicitSelection?: boolean;
}

export function nativeShareAddressUnavailableReason(
  advertisedEndpoints: ReadonlyArray<AdvertisedEndpoint>,
): "no-private-address" | "no-private-default-route" | null {
  const privateEndpoints = advertisedEndpoints.filter(
    (endpoint) =>
      shareClassForPairingEndpoint(endpoint.httpBaseUrl) === "off-host" &&
      endpoint.source === "desktop-core" &&
      (endpoint.reachability === "lan" || endpoint.reachability === "private-network"),
  );
  if (privateEndpoints.some((endpoint) => endpoint.isDefault === true)) return null;
  return privateEndpoints.length === 0 ? "no-private-address" : "no-private-default-route";
}

export function resolveShareAddressOptions(input: {
  readonly intent: ShareIntent;
  readonly advertisedEndpoints: ReadonlyArray<AdvertisedEndpoint>;
  readonly exposureState: DesktopServerExposureState | null;
  readonly primaryHttpBaseUrl: string | null;
}): ReadonlyArray<ShareAddressOption> {
  if (input.intent === "this-computer") {
    return input.primaryHttpBaseUrl === null
      ? []
      : [
          {
            id: "primary",
            label: "This computer",
            httpBaseUrl: input.primaryHttpBaseUrl,
            description: "Only clients on this machine (or a tunnel into it) can use this offer.",
          },
        ];
  }

  if (input.intent === "custom") {
    return [{ id: "custom", label: "Custom address", httpBaseUrl: null }];
  }

  const options: ShareAddressOption[] = [];
  const automaticEndpoint =
    input.exposureState === null
      ? input.primaryHttpBaseUrl
      : input.exposureState.mode === "network-accessible"
        ? input.exposureState.endpointUrl
        : null;
  const offHostEndpoints = input.advertisedEndpoints.filter(
    (endpoint) => shareClassForPairingEndpoint(endpoint.httpBaseUrl) === "off-host",
  );
  const availableOffHostEndpoints = offHostEndpoints.filter(
    (endpoint) => endpoint.status === "available",
  );
  const nativeManaged = input.exposureState?.management === "native";
  const nativePrivateDefaultObserved =
    nativeShareAddressUnavailableReason(offHostEndpoints) === null;
  const selectableOffHostEndpoints = availableOffHostEndpoints.filter(
    (endpoint) => !(nativeManaged && endpoint.reachability === "public"),
  );
  const canOfferAutomaticLan =
    (input.exposureState === null && automaticEndpoint !== null) ||
    (nativeManaged && input.exposureState?.mode === "local-only" && nativePrivateDefaultObserved) ||
    (automaticEndpoint !== null &&
      classifyPairingEndpoint(automaticEndpoint) === "private-network");
  if (canOfferAutomaticLan) {
    options.push({
      id: "auto-lan",
      label: "Automatic (LAN)",
      httpBaseUrl: automaticEndpoint,
      description:
        input.exposureState?.endpointUrl === null
          ? "Coddy Studio will enable remote access and choose a LAN address."
          : "Use the LAN address selected by Coddy Studio.",
    });
  }
  const seen = new Set(
    canOfferAutomaticLan && automaticEndpoint !== null ? [automaticEndpoint] : [],
  );
  for (const endpoint of selectableOffHostEndpoints) {
    if (seen.has(endpoint.httpBaseUrl)) {
      continue;
    }
    seen.add(endpoint.httpBaseUrl);
    options.push({
      id: endpoint.id,
      label: new URL(endpoint.httpBaseUrl).hostname,
      httpBaseUrl: endpoint.httpBaseUrl,
      ...(endpoint.reachability === "public" ? { requiresExplicitSelection: true } : {}),
      ...(endpoint.description === undefined ? {} : { description: endpoint.description }),
    });
  }
  return options;
}

export type ShareOfferCleanupOutcome =
  | "local-confirmed"
  | "active-reason"
  | "cancellation-unconfirmed"
  | "cleanup-failed";

export type GenerateShareOfferFailure =
  | { readonly kind: "invalid-address"; readonly message: string }
  | { readonly kind: "widen-failed"; readonly message: string }
  | {
      readonly kind: "mint-failed";
      readonly message: string;
      readonly widened: boolean;
      readonly cleanup: ShareOfferCleanupOutcome;
    };

export interface GeneratedShareOffer {
  readonly code: string;
  readonly deepLink: string;
  readonly browserUrl: string;
  readonly endpoint: string;
  readonly name: string;
  readonly expiresAt: string;
  readonly reach: ShareIntent;
  readonly endpointClass: "loopback" | "off-host";
}

export interface GenerateShareOfferDeps {
  readonly intent: ShareIntent;
  readonly name: string;
  readonly customAddress: string | null;
  readonly selectedOption: ShareAddressOption;
  readonly hasDesktopBridge: boolean;
  readonly exposureState: DesktopServerExposureState | null;
  readonly applyServerExposure:
    | ((desired: "local-only" | "network-accessible") => Promise<DesktopServerExposureState>)
    | null;
  readonly mintOffer: (input: {
    name: string;
    endpoint: string;
    reach: ShareIntent;
    idempotencyKey: string;
  }) => Promise<{ code: string; endpoint: string; name: string; expiresAt: string }>;
  readonly newIdempotencyKey: () => string;
  readonly classifyMintError: (error: unknown) => "retryable" | "fatal";
  readonly cancelOffer: (idempotencyKey: string) => Promise<void>;
  readonly cleanupExposureAfterFailedMint:
    | null
    | (() => Promise<"local-confirmed" | "active-reason">);
  readonly sleep: (ms: number) => Promise<void>;
  readonly requestTimeoutMs: number;
}

class ShareOfferOperationTimeoutError extends Error {
  constructor(operation: string, timeoutMs: number) {
    super(`${operation} timed out after ${String(timeoutMs)}ms.`);
    this.name = "ShareOfferOperationTimeoutError";
  }
}

async function withRequestTimeout<A>(
  operation: string,
  timeoutMs: number,
  request: Promise<A>,
): Promise<A> {
  return raceWithDeadline(
    request,
    timeoutMs,
    () => new ShareOfferOperationTimeoutError(operation, timeoutMs),
  );
}

function resolveOfferEndpoint(deps: GenerateShareOfferDeps): string | null {
  if (deps.intent === "custom") {
    const customAddress = deps.customAddress?.trim();
    if (!customAddress) throw new Error("Enter a valid http(s) address.");
    try {
      return normalizeHttpBaseUrl(customAddress);
    } catch {
      throw new Error("Enter a valid http(s) address.");
    }
  }
  return deps.selectedOption.httpBaseUrl;
}

export async function generateShareOffer(
  deps: GenerateShareOfferDeps,
): Promise<
  { ok: true; offer: GeneratedShareOffer } | { ok: false; failure: GenerateShareOfferFailure }
> {
  let endpoint: string | null;
  try {
    endpoint = resolveOfferEndpoint(deps);
  } catch (error) {
    return {
      ok: false,
      failure: {
        kind: "invalid-address",
        message: error instanceof Error ? error.message : "Enter a valid http(s) address.",
      },
    };
  }

  const endpointClassForWiden =
    deps.intent === "custom" && endpoint !== null
      ? shareClassForPairingEndpoint(endpoint)
      : deps.intent === "another-device"
        ? "off-host"
        : "loopback";
  if (endpointClassForWiden === "unconnectable") {
    return {
      ok: false,
      failure: {
        kind: "invalid-address",
        message: "This address is not reachable as entered. Check the host and port.",
      },
    };
  }

  let widened = false;
  if (
    deps.intent === "another-device" &&
    endpointClassForWiden === "off-host" &&
    deps.hasDesktopBridge &&
    deps.applyServerExposure !== null &&
    deps.exposureState?.mode !== "network-accessible"
  ) {
    try {
      const state = await deps.applyServerExposure("network-accessible");
      widened = true;
      if (endpoint === null) endpoint = state.endpointUrl;
    } catch (error) {
      return {
        ok: false,
        failure: {
          kind: "widen-failed",
          message: error instanceof Error ? error.message : "Could not enable remote access.",
        },
      };
    }
  }

  if (endpoint === null) endpoint = deps.exposureState?.endpointUrl ?? null;
  if (endpoint === null) {
    return {
      ok: false,
      failure: { kind: "widen-failed", message: "No reachable network address is available." },
    };
  }

  const idempotencyKey = deps.newIdempotencyKey();
  const maxMintAttempts = 5;
  let lastError: unknown = null;
  for (let attempt = 0; attempt < maxMintAttempts; attempt += 1) {
    if (attempt > 0) await deps.sleep(2_000);
    try {
      const minted = await withRequestTimeout(
        "Pairing-offer creation",
        deps.requestTimeoutMs,
        deps.mintOffer({
          name: deps.name,
          endpoint,
          reach: deps.intent,
          idempotencyKey,
        }),
      );
      const endpointClass = shareClassForPairingEndpoint(minted.endpoint);
      return {
        ok: true,
        offer: {
          code: minted.code,
          deepLink: buildPairingDeepLink(minted.code),
          browserUrl: buildBrowserPairUrl(minted.endpoint, minted.code),
          endpoint: minted.endpoint,
          name: minted.name,
          expiresAt: minted.expiresAt,
          reach: deps.intent,
          endpointClass: endpointClass === "unconnectable" ? "off-host" : endpointClass,
        },
      };
    } catch (error) {
      lastError = error;
      if (deps.classifyMintError(error) === "fatal") break;
    }
  }

  let cleanup: ShareOfferCleanupOutcome =
    deps.exposureState?.mode === "local-only" ? "local-confirmed" : "active-reason";
  let message =
    lastError instanceof Error ? lastError.message : "Could not create the pairing offer.";
  let cancellationSucceeded = false;
  let cancellationError: unknown = null;
  const maxCancellationAttempts = 3;
  for (let attempt = 0; attempt < maxCancellationAttempts; attempt += 1) {
    if (attempt > 0) await deps.sleep(2_000);
    try {
      await withRequestTimeout(
        "Pairing-offer cancellation",
        deps.requestTimeoutMs,
        deps.cancelOffer(idempotencyKey),
      );
      cancellationSucceeded = true;
      break;
    } catch (error) {
      cancellationError = error;
    }
  }
  if (!cancellationSucceeded) {
    cleanup = "cancellation-unconfirmed";
    const cancellationMessage =
      cancellationError instanceof Error ? cancellationError.message : String(cancellationError);
    message = `${message} Pairing-offer cancellation failed: ${cancellationMessage}`;
  }
  if (widened && deps.cleanupExposureAfterFailedMint !== null) {
    // The cleanup consults the authoritative share state, so it is safe to
    // run even when the cancellation is unconfirmed: a still-live offer keeps
    // the host wide as an active reason, while a cancelled or consumed one
    // narrows instead of leaving the host bound to 0.0.0.0 with an open
    // firewall until the app restarts.
    try {
      cleanup = await withRequestTimeout(
        "Remote-access reconciliation",
        deps.requestTimeoutMs,
        deps.cleanupExposureAfterFailedMint(),
      );
    } catch (error) {
      cleanup = cancellationSucceeded ? "cleanup-failed" : "cancellation-unconfirmed";
      const cleanupMessage = error instanceof Error ? error.message : String(error);
      message = `${message} Remote-access cleanup failed: ${cleanupMessage}`;
    }
  } else if (widened && cancellationSucceeded) {
    cleanup = "cleanup-failed";
    message = `${message} Remote-access cleanup could not be verified.`;
  }

  return {
    ok: false,
    failure: {
      kind: "mint-failed",
      widened,
      cleanup,
      message,
    },
  };
}
