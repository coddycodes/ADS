import {
  type AtomCommandResult,
  squashAtomCommandFailure,
} from "@bibcode/client-runtime/state/runtime";
import type { DesktopSshEnvironmentTarget, EnvironmentId, ThreadId } from "@bibcode/contracts";
import * as Option from "effect/Option";

import { environmentCatalog } from "~/connection/catalog";
import { appAtomRegistry } from "~/rpc/atomRegistry";
import { readPreparedConnection } from "~/state/session";

import { isBrowserMode } from "~/components/preview/previewBridge";

import { formatHost, resolvePreviewTarget, UNREACHABLE_MESSAGES } from "./browserTargetResolver";
import { showSamePortBusyNotice } from "./linkNotices";

export type GatewayOpenMutation = (input: {
  environmentId: EnvironmentId;
  input: { threadId: ThreadId; url: string };
}) => Promise<
  AtomCommandResult<{ gatewayPort: number; capability: string; expiresAtMs: number }, unknown>
>;

export type PreviewNavigationResolution =
  | { readonly kind: "ok"; readonly url: string }
  | {
      readonly kind: "unreachable";
      readonly message: string;
      /**
       * The server's gateway refused the URL, so every client sees the same
       * failure. Otherwise the cause is this client's own reach (SSH forward,
       * connection, session, transport) and must stay out of shared preview state.
       */
      readonly refusedByServer?: true;
      /** Transient: the same request may succeed if tried again (unlike HTTPS or a stopped server). */
      readonly retryable?: true;
    };

const BOOTSTRAP_PATH = "/__bibcode/bootstrap";

/** A tab whose host unmounted keeps its forwards this long, so switching back reuses them. */
export const TAB_RELEASE_GRACE_MS = 60_000;
/** An open handed to the system browser has no tab to end with; its forward lives this long. */
export const UNTRACKED_LEASE_MS = 5 * 60_000;

/** The desktop bridge's exact rejection when the environment's SSH tunnel is down. */
const SSH_NOT_ACTIVE = "SSH connection is not active.";

/** Client-facing gateway origin -> canonical origin (`http://localhost:<port>`). */
const canonicalOrigins = new Map<string, string>();

interface SshForward {
  readonly target: DesktopSshEnvironmentTarget;
  readonly gatewayPort: number;
}

/**
 * One thread's canonical origin over SSH and the forward its holders share
 * (the server keeps one gateway listener per thread and upstream port). A
 * holder is a tab or the timed lease of an untracked open. The forward lives
 * while a holder remains or a resolution is still pending.
 */
interface SshForwardEntry {
  forward: SshForward | null;
  readonly holders: Set<string>;
  pending: number;
  /**
   * Every local client origin this entry installed, kept until the entry goes:
   * a tab's native history can go Back to a replaced forward's origin.
   */
  readonly origins: Set<string>;
  /**
   * The release of a forward whose gateway listener the server replaced. It
   * may still hold the local port the replacement wants, so a resolution
   * waits for it before asking for that port.
   */
  retiring: Promise<void> | null;
}

/** Per environment: `<threadId> <canonical origin>` -> forward bookkeeping. */
const sshForwards = new Map<EnvironmentId, Map<string, SshForwardEntry>>();

/** Bumped when a tab is released, so a resolution that outlives its tab never holds a forward. */
const tabGenerations = new Map<string, number>();

/** Grace releases waiting for tabs whose host unmounted. */
const tabReleaseTimers = new Map<string, ReturnType<typeof setTimeout>>();

/** Running leases of untracked opens. */
const leaseTimers = new Set<ReturnType<typeof setTimeout>>();
let nextLeaseId = 0;

/** The entry that last installed each local client origin. */
const originOwners = new Map<string, SshForwardEntry>();

/**
 * Ports of 1024 or more that browser engines refuse to load (the Fetch
 * standard's "bad ports"). A forward on one could never show the page, so they
 * keep a random local port.
 */
const BROWSER_BLOCKED_PORTS = new Set([
  1719, 1720, 1723, 2049, 3659, 4045, 4190, 5060, 5061, 6000, 6566, 6665, 6666, 6667, 6668, 6669,
  6679, 6697, 10080,
]);

/** `<environmentId> <canonical origin>` pairs whose busy-port note was shown this session. */
const samePortNotices = new Set<string>();

function releaseForward(forward: SshForward): void {
  void window.desktopBridge
    ?.releaseSshForward(forward.target, forward.gatewayPort)
    .catch(() => undefined);
}

function readSshTarget(environmentId: EnvironmentId): DesktopSshEnvironmentTarget | null {
  const profile = appAtomRegistry
    .get(environmentCatalog.catalogValueAtom)
    .entries.get(environmentId)?.profile;
  return profile !== undefined &&
    Option.isSome(profile) &&
    profile.value._tag === "SshConnectionProfile"
    ? profile.value.target
    : null;
}

function openEntry(environmentId: EnvironmentId, key: string): SshForwardEntry {
  let entries = sshForwards.get(environmentId);
  if (!entries) {
    entries = new Map();
    sshForwards.set(environmentId, entries);
  }
  let entry = entries.get(key);
  if (!entry) {
    entry = { forward: null, holders: new Set(), pending: 0, origins: new Set(), retiring: null };
    entries.set(key, entry);
  }
  return entry;
}

function releaseIfUnused(environmentId: EnvironmentId, key: string, entry: SshForwardEntry): void {
  if (entry.holders.size > 0 || entry.pending > 0) return;
  if (sshForwards.get(environmentId)?.get(key) === entry)
    sshForwards.get(environmentId)?.delete(key);
  // A newer entry that reused one of these local ports keeps its mapping.
  for (const origin of entry.origins) {
    if (originOwners.get(origin) !== entry) continue;
    originOwners.delete(origin);
    canonicalOrigins.delete(origin);
  }
  if (entry.forward) releaseForward(entry.forward);
}

function releaseHolder(environmentId: EnvironmentId, holder: string): void {
  for (const [key, entry] of sshForwards.get(environmentId) ?? []) {
    if (entry.holders.delete(holder)) releaseIfUnused(environmentId, key, entry);
  }
}

/**
 * Stops tracking a tab now (its shared preview tab closed) and releases each
 * SSH forward nothing else holds. A tab holds every origin it has shown until
 * then, because its native history can go Back to any of them without
 * resolving again.
 */
export function releasePreviewTab(environmentId: EnvironmentId, tabId: string): void {
  retainPreviewTab(tabId);
  tabGenerations.set(tabId, (tabGenerations.get(tabId) ?? 0) + 1);
  releaseHolder(environmentId, tabId);
}

/** The tab's host unmounted: release the tab unless it mounts again within the grace period. */
export function releasePreviewTabAfterGrace(environmentId: EnvironmentId, tabId: string): void {
  if (tabReleaseTimers.has(tabId)) return;
  const timer = setTimeout(() => releasePreviewTab(environmentId, tabId), TAB_RELEASE_GRACE_MS);
  tabReleaseTimers.set(tabId, timer);
}

/** The tab's host mounted again: cancel its grace release and keep its forwards. */
export function retainPreviewTab(tabId: string): void {
  const timer = tabReleaseTimers.get(tabId);
  if (timer === undefined) return;
  clearTimeout(timer);
  tabReleaseTimers.delete(tabId);
}

/** Resolve a canonical preview URL to the URL this client's webview should load. */
export async function resolveForNavigation(input: {
  environmentId: EnvironmentId;
  threadId: ThreadId;
  canonicalUrl: string;
  gatewayOpen: GatewayOpenMutation;
  /**
   * The preview tab that will load the URL; ties an SSH forward to the tab's
   * lifetime. Without one (the system browser), the forward is leased for
   * `UNTRACKED_LEASE_MS`.
   */
  tabId?: string;
}): Promise<PreviewNavigationResolution> {
  const { environmentId, tabId } = input;
  const resolution = resolvePreviewTarget(environmentId, input.canonicalUrl);
  if (resolution.kind === "unreachable") {
    return {
      kind: "unreachable",
      message: UNREACHABLE_MESSAGES[resolution.reason](resolution.environmentLabel),
    };
  }
  if (resolution.kind === "reachable") return { kind: "ok", url: resolution.url };

  const canonical = new URL(resolution.url);
  const connectionLabel = readPreparedConnection(environmentId)?.label;
  // Used mid-sentence; only the "isn't connected" copy starts with it.
  const label = connectionLabel ?? "this environment";
  // Only the desktop host can run the SSH forward; refuse before the server admits a target.
  if (resolution.via === "ssh" && isBrowserMode()) {
    return { kind: "unreachable", message: UNREACHABLE_MESSAGES.ssh(label) };
  }
  // Opened before any await so a close or a sibling's failure during this
  // resolution cannot release the forward it is about to use.
  const key = `${input.threadId} ${canonical.origin}`;
  const entry = resolution.via === "ssh" ? openEntry(environmentId, key) : null;
  const generation = tabId === undefined ? 0 : (tabGenerations.get(tabId) ?? 0);
  if (entry) entry.pending += 1;
  const settle = (forward: SshForward | null, clientOrigin?: string) => {
    if (!entry) return;
    entry.pending -= 1;
    if (forward && clientOrigin !== undefined) {
      if (entry.forward && entry.forward.gatewayPort !== forward.gatewayPort) {
        // The server replaced the listener; the old forward leads nowhere.
        releaseForward(entry.forward);
      }
      entry.forward = forward;
      entry.origins.add(clientOrigin);
      originOwners.set(clientOrigin, entry);
      if (tabId === undefined) holdForUntrackedOpen(environmentId, key, entry);
      else if ((tabGenerations.get(tabId) ?? 0) === generation) entry.holders.add(tabId);
    }
    releaseIfUnused(environmentId, key, entry);
  };
  const fail = (failure: GatewayFailure): PreviewNavigationResolution => {
    settle(null);
    return { kind: "unreachable", ...failure };
  };

  const result = await input.gatewayOpen({
    environmentId,
    input: { threadId: input.threadId, url: resolution.url },
  });
  if (result._tag === "Failure") return fail(gatewayFailure(result, label, canonical));
  const { gatewayPort, capability } = result.value;

  let clientOrigin: string;
  if (resolution.via === "host") {
    clientOrigin = `http://${formatHost(resolution.host)}:${gatewayPort}`;
    canonicalOrigins.set(clientOrigin, canonical.origin);
  } else {
    const bridge = window.desktopBridge;
    if (!bridge) return fail({ message: UNREACHABLE_MESSAGES.ssh(label) });
    const target = readSshTarget(environmentId);
    if (!target) return fail({ message: retryMessage(label), retryable: true });
    // The same local port keeps the page on its real origin (OAuth redirects,
    // CORS allowlists); ports below 1024 need privileges on this computer.
    const canonicalPort = Number(canonical.port);
    const preferred =
      canonicalPort >= 1024 && !BROWSER_BLOCKED_PORTS.has(canonicalPort)
        ? canonicalPort
        : undefined;
    if (entry?.forward && entry.forward.gatewayPort !== gatewayPort) {
      // The server replaced the listener (it closes idle ones); the old
      // forward leads nowhere but may still hold the preferred local port.
      const replaced = entry.forward;
      entry.forward = null;
      entry.retiring = bridge
        .releaseSshForward(replaced.target, replaced.gatewayPort)
        .catch(() => undefined);
    }
    await entry?.retiring;
    let localPort: number;
    try {
      // Forwards end when the managed tunnel reconnects; the bridge call is
      // idempotent while one is alive, so every navigation re-establishes it.
      localPort = await bridge.sshForward(target, gatewayPort, preferred);
    } catch (cause) {
      return fail({
        message: isSshNotActive(cause)
          ? UNREACHABLE_MESSAGES.disconnected(connectionLabel ?? "This environment")
          : retryMessage(label),
        retryable: true,
      });
    }
    if (localPort === preferred) {
      clientOrigin = canonical.origin;
    } else {
      clientOrigin = `http://127.0.0.1:${localPort}`;
      const noticeKey = `${environmentId} ${canonical.origin}`;
      if (preferred !== undefined && !samePortNotices.has(noticeKey)) {
        samePortNotices.add(noticeKey);
        showSamePortBusyNotice(canonical.host);
      }
    }
    // Mapped before settling: settling may release (and forget) it at once.
    canonicalOrigins.set(clientOrigin, canonical.origin);
    settle({ target, gatewayPort }, clientOrigin);
  }

  const to = `${canonical.pathname}${canonical.search}${canonical.hash}`;
  // Browser mode names this page so the gateway lets it frame the preview.
  const ui = isBrowserMode() ? `&ui=${encodeURIComponent(location.origin)}` : "";
  return {
    kind: "ok",
    url: `${clientOrigin}${BOOTSTRAP_PATH}?cap=${encodeURIComponent(capability)}&to=${encodeURIComponent(to)}${ui}`,
  };
}

/** An open with no tab (the system browser) holds its forward for a fixed time. */
function holdForUntrackedOpen(
  environmentId: EnvironmentId,
  key: string,
  entry: SshForwardEntry,
): void {
  const holder = `lease:${++nextLeaseId}`;
  entry.holders.add(holder);
  const timer = setTimeout(() => {
    leaseTimers.delete(timer);
    entry.holders.delete(holder);
    releaseIfUnused(environmentId, key, entry);
  }, UNTRACKED_LEASE_MS);
  leaseTimers.add(timer);
}

function isSshNotActive(cause: unknown): boolean {
  const message = cause instanceof Error ? cause.message : cause;
  return message === SSH_NOT_ACTIVE;
}

type GatewayFailure = {
  readonly message: string;
  readonly refusedByServer?: true;
  readonly retryable?: true;
};

const retryMessage = (label: string) =>
  `Couldn't open a preview connection to ${label}. Try again, or reconnect ${label} if it keeps failing.`;

function gatewayFailure(
  result: Parameters<typeof squashAtomCommandFailure>[0],
  label: string,
  canonical: URL,
): GatewayFailure {
  const error = squashAtomCommandFailure(result) as {
    _tag?: unknown;
    reason?: unknown;
    message?: unknown;
  } | null;
  if (error?._tag !== "PreviewGatewayError") {
    return { message: retryMessage(label), retryable: true };
  }
  const refused = (message: string): GatewayFailure => ({ message, refusedByServer: true });
  switch (error.reason) {
    case "https-unsupported":
      return refused(
        `HTTPS dev servers can't be previewed through the gateway yet; serve over HTTP or open it on ${label} directly.`,
      );
    case "not-admitted":
      return refused(`BiBCode can only preview plain HTTP addresses on ${label}'s own localhost.`);
    case "no-upstream":
      // A stopped dev server is recoverable; say so instead of "unsupported".
      return refused(`Nothing is listening on port ${canonical.port || "80"} on ${label}.`);
    case "not-reachable":
      // Depends on the address this client used (public or proxied), so it
      // stays local: a client on the LAN may load the same tab fine.
      return { message: `${error.message} Open it on ${label} directly.` };
    default:
      // This client's expired session, a gateway shutdown or a bind failure:
      // another client may load the page fine, and a retry may too.
      return { message: retryMessage(label), retryable: true };
  }
}

/** True for this client's gateway bootstrap page, which immediately replaces itself. */
export function isGatewayBootstrapUrl(url: string | null): boolean {
  if (url === null) return false;
  try {
    const parsed = new URL(url);
    return parsed.pathname === BOOTSTRAP_PATH && canonicalOrigins.has(parsed.origin);
  } catch {
    return false;
  }
}

/** True for a URL on one of this client's gateway origins (a LAN address or an SSH forward). */
export function isGatewayClientUrl(url: string): boolean {
  try {
    return canonicalOrigins.has(new URL(url).origin);
  } catch {
    return false;
  }
}

/** Map a client-side gateway URL back to its canonical http://localhost:<port> form; other URLs pass through. */
export function canonicalizePreviewUrl(url: string): string {
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return url;
  }
  const canonicalOrigin = canonicalOrigins.get(parsed.origin);
  if (canonicalOrigin === undefined) return url;
  if (parsed.pathname === BOOTSTRAP_PATH) {
    const to = parsed.searchParams.get("to");
    return `${canonicalOrigin}${to?.startsWith("/") ? to : "/"}`;
  }
  return `${canonicalOrigin}${parsed.pathname}${parsed.search}${parsed.hash}`;
}

/** Test seam. */
export function resetPreviewGatewayForTests(): void {
  for (const timer of [...tabReleaseTimers.values(), ...leaseTimers]) clearTimeout(timer);
  tabReleaseTimers.clear();
  leaseTimers.clear();
  canonicalOrigins.clear();
  originOwners.clear();
  samePortNotices.clear();
  sshForwards.clear();
  tabGenerations.clear();
}
