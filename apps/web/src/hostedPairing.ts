import {
  readHostedPairingRequest as readNormalizedHostedPairingRequest,
  type HostedPairingRequest,
} from "@coddy/shared/remote";

import { setPairingTokenOnUrl } from "./pairingUrl";

export type { HostedPairingRequest };

export type HostedAppChannel = "latest" | "nightly";

export function configuredHostedAppUrl(): string {
  const configured = import.meta.env.VITE_HOSTED_APP_URL?.trim();
  if (configured) return configured;

  const origin = typeof window === "undefined" ? "" : window.location.origin;
  return origin && origin !== "null" ? origin : "http://localhost";
}

function configuredBackendUrl(): string {
  return import.meta.env.VITE_HTTP_URL?.trim() || import.meta.env.VITE_WS_URL?.trim() || "";
}

function configuredHostedAppChannel(): HostedAppChannel | null {
  const channel = import.meta.env.VITE_HOSTED_APP_CHANNEL?.trim().toLowerCase();
  return channel === "latest" || channel === "nightly" ? channel : null;
}

function originFromUrl(value: string): string | null {
  try {
    return new URL(value).origin;
  } catch {
    return null;
  }
}

/**
 * Whether this page is the web app a Coddy Studio server served for this very host: the server writes
 * `<meta name="coddy-studio-served-host">` into the entry document it serves. Only that document can
 * carry it, and a copy deployed on another host names a different host, so the marker cannot turn
 * a remote client's page into one that pairs with its own origin.
 */
export function isServedByThisBackend(url: URL = new URL(window.location.href)): boolean {
  const marker =
    typeof document === "undefined"
      ? null
      : (document.querySelector('meta[name="coddy-studio-served-host"]')?.getAttribute("content") ??
        null);
  if (marker === null) return false;
  // Compare as the browser would: a forwarded `host:443` names the same host as `host`.
  let markerHost: string;
  try {
    markerHost = new URL(`${url.protocol}//${marker}`).host;
  } catch {
    return false;
  }
  return markerHost.toLowerCase() === url.host.toLowerCase();
}

let servedSessionStalled = false;

/**
 * This page's own server did not answer its session check in time, and the page fell back to the
 * other servers it is paired with. Later navigations keep that mode instead of waiting out the
 * same stalled check again, until the page reloads or pairs its own server.
 */
export function setServedSessionStalled(stalled: boolean): void {
  servedSessionStalled = stalled;
}

export function isServedSessionStalled(): boolean {
  return servedSessionStalled;
}

/**
 * Whether this is the hosted static app, a remote client with no backend of its own. A page a
 * server served for this host is not; without that marker an unconfigured build takes its own
 * origin for the hosted app, as before.
 */
export function isHostedStaticApp(url: URL = new URL(window.location.href)): boolean {
  return !isServedByThisBackend(url) && matchesHostedApp(url);
}

/**
 * Whether the host-based pairing links this build makes (`buildHostedPairingUrl`) land on this app:
 * the configured hosted URL, or this origin when none is configured, a served page included.
 */
export function isHostedPairingApp(url: URL = new URL(window.location.href)): boolean {
  return matchesHostedApp(url);
}

function matchesHostedApp(url: URL): boolean {
  if (typeof window !== "undefined" && window.desktopBridge !== undefined) {
    return false;
  }

  if (configuredBackendUrl()) {
    return false;
  }

  if (configuredHostedAppChannel()) {
    return true;
  }

  const hostedOrigin = originFromUrl(configuredHostedAppUrl());
  return hostedOrigin !== null && url.origin === hostedOrigin;
}

export function readHostedPairingRequest(url: URL = new URL(window.location.href)) {
  return readNormalizedHostedPairingRequest(url);
}

export function hasHostedPairingRequest(url: URL = new URL(window.location.href)): boolean {
  return readHostedPairingRequest(url) !== null;
}

export function buildHostedPairingUrl(input: {
  readonly host: string;
  readonly token: string;
  readonly label?: string | null;
}): string {
  const url = new URL("/pair", configuredHostedAppUrl());
  url.searchParams.set("host", input.host);

  const label = input.label?.trim();
  if (label) {
    url.searchParams.set("label", label);
  }

  return setPairingTokenOnUrl(url, input.token).toString();
}

export function buildHostedChannelSelectionUrl(input: {
  readonly channel: HostedAppChannel;
}): string {
  const url = new URL("/__coddy-studio/channel", configuredHostedAppUrl());
  url.searchParams.set("channel", input.channel);
  return url.toString();
}

let servedPrimarySuppressed: boolean | null = null;
const servedPrimarySuppressionListeners = new Set<() => void>();

/**
 * Whether the platform source withholds this page's own server. A page this server served
 * withholds it until the root route confirms it is paired (or the page pairs it), so an unpaired
 * server is never offered as an environment that cannot connect, not even one discovered before
 * that check. Every other page never withholds it.
 */
export function isServedPrimarySuppressed(): boolean {
  if (servedPrimarySuppressed !== null) return servedPrimarySuppressed;
  return typeof window !== "undefined" && isServedByThisBackend();
}

/** Set by the root route; listeners rebuild the platform's environments at once. */
export function setServedPrimarySuppressed(suppressed: boolean): void {
  const before = isServedPrimarySuppressed();
  servedPrimarySuppressed = suppressed;
  if (before === suppressed) return;
  for (const listener of servedPrimarySuppressionListeners) listener();
}

export function onServedPrimarySuppressionChange(listener: () => void): () => void {
  servedPrimarySuppressionListeners.add(listener);
  return () => {
    servedPrimarySuppressionListeners.delete(listener);
  };
}

export function resetServedPrimarySuppressionForTests(): void {
  servedPrimarySuppressed = null;
  servedPrimarySuppressionListeners.clear();
}
