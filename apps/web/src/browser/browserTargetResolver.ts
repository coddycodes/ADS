import type {
  BrowserNavigationTarget,
  EnvironmentId,
  PreviewUrlResolution,
} from "@bibcode/contracts";
import { normalizePreviewUrl } from "@bibcode/shared/preview";

import { canFrameGatewayPreviews } from "~/components/preview/previewBridge";
import { readPreparedConnection } from "~/state/session";

const isPrivateNetworkHost = (host: string): boolean => {
  const normalized = host.toLowerCase().replace(/^\[|\]$/g, "");
  if (normalized === "localhost" || normalized === "::1" || normalized.endsWith(".local")) {
    return true;
  }
  if (normalized.endsWith(".ts.net")) return true;
  // IPv6 unique-local (fc00::/7) and link-local (fe80::/10).
  if (/^f[cd][0-9a-f]{2}:/.test(normalized) || /^fe[89ab][0-9a-f]:/.test(normalized)) return true;
  const parts = normalized.split(".").map(Number);
  if (parts.length !== 4 || parts.some((part) => !Number.isInteger(part))) return false;
  return (
    parts[0] === 10 ||
    (parts[0] === 172 && parts[1]! >= 16 && parts[1]! <= 31) ||
    (parts[0] === 192 && parts[1] === 168) ||
    (parts[0] === 100 && parts[1]! >= 64 && parts[1]! <= 127) || // tailnet CGNAT 100.64.0.0/10
    parts[0] === 127 ||
    (parts[0] === 169 && parts[1] === 254)
  );
};

const unbracket = (host: string) => host.toLowerCase().replace(/^\[|\]$/g, "");

/** Any address that names the machine it is resolved on, including wildcard binds. */
const isLoopbackHost = (hostname: string): boolean => {
  const host = unbracket(hostname);
  return (
    host === "localhost" ||
    host.endsWith(".localhost") ||
    /^127\.\d+\.\d+\.\d+$/.test(host) ||
    host === "0.0.0.0" ||
    host === "::1" ||
    host === "::"
  );
};

const isWildcardHost = (hostname: string) => {
  const host = unbracket(hostname);
  return host === "0.0.0.0" || host === "::";
};

const isIpLiteral = (host: string) => host.includes(":") || /^\d+\.\d+\.\d+\.\d+$/.test(host);

const parseUrl = (raw: string): URL | null => {
  try {
    return new URL(raw);
  } catch {
    return null;
  }
};

const parsePreviewUrl = (raw: string): URL | null => {
  try {
    return new URL(normalizePreviewUrl(raw));
  } catch {
    return null;
  }
};

export type PreviewUnreachableReason = "disconnected" | "ssh" | "relay" | "public-host";
export type PreviewTargetResolution =
  | { readonly kind: "reachable"; readonly url: string }
  /**
   * A server-loopback address reached through the environment's preview
   * gateway. `url` is canonical (`http://localhost:<port>/…`; a same-host
   * environment framed in browser mode keeps its loopback as a direct open
   * would): it is what shared preview state stores, and each client resolves it
   * for its own webview.
   */
  | {
      readonly kind: "gateway";
      readonly url: string;
      readonly via: "host";
      readonly host: string;
      /** A same-host page framed in browser mode: outside the frame it opens directly. */
      readonly direct?: true;
    }
  | { readonly kind: "gateway"; readonly url: string; readonly via: "ssh" }
  | {
      readonly kind: "unreachable";
      readonly reason: PreviewUnreachableReason;
      readonly environmentLabel: string;
      /** The normalized address that could not be opened. */
      readonly url: string;
    };

const UNREACHABLE_REMOTE = (label: string) =>
  `This address is on ${label}, not this computer. Opening its ports from here isn't supported yet.`;

export const UNREACHABLE_MESSAGES: Record<PreviewUnreachableReason, (label: string) => string> = {
  disconnected: (label) => `${label} isn't connected. Reconnect it, then open the link again.`,
  ssh: UNREACHABLE_REMOTE,
  relay: UNREACHABLE_REMOTE,
  "public-host": UNREACHABLE_REMOTE,
};

type EnvironmentReach =
  /** `host`: the address this client reaches the environment's server on. */
  | { readonly kind: "same-host"; readonly host: string }
  | { readonly kind: "host"; readonly host: string }
  | { readonly kind: "ssh" }
  | {
      readonly kind: "unreachable";
      readonly reason: PreviewUnreachableReason;
      readonly label: string;
    };

// The connection target alone never decides "same machine": a primary can be a
// browser served by a remote host, and desktop-local WSL is a bearer target on
// the VM address. SSH and relay endpoints are never the environment's own host;
// SSH reaches the gateway through a desktop-managed forward.
function classifyEnvironmentReach(environmentId: EnvironmentId): EnvironmentReach {
  const connection = readPreparedConnection(environmentId);
  if (!connection) {
    return { kind: "unreachable", reason: "disconnected", label: "This environment" };
  }
  const label = connection.label;
  const baseUrl = parseUrl(connection.httpBaseUrl);
  if (!baseUrl) return { kind: "unreachable", reason: "disconnected", label };
  if (connection.target._tag === "SshConnectionTarget") return { kind: "ssh" };
  if (connection.target._tag === "RelayConnectionTarget") {
    return { kind: "unreachable", reason: "relay", label };
  }
  const host = unbracket(baseUrl.hostname);
  if (isLoopbackHost(host)) return { kind: "same-host", host };
  // A host name (devbox, *.lan, *.internal) is the user's own name for the
  // machine; only a public IP literal is known to be off the private network.
  if (!isIpLiteral(host) || isPrivateNetworkHost(host)) return { kind: "host", host };
  return { kind: "unreachable", reason: "public-host", label };
}

/** Brackets an IPv6 literal for use in a URL authority. */
export const formatHost = (host: string) => (host.includes(":") ? `[${host}]` : host);

/**
 * Resolves an agent navigation target to the URL shared preview state stores.
 * A gateway-backed `resolvedUrl` stays canonical; the native view resolves it
 * per client with `resolveForNavigation`.
 */
export function resolveBrowserNavigationTarget(
  environmentId: EnvironmentId,
  target: BrowserNavigationTarget,
): PreviewUrlResolution {
  let requestedUrl: string;
  if (target.kind === "url") {
    requestedUrl = target.url;
  } else {
    const protocol = target.protocol ?? "http";
    const path = target.path?.startsWith("/") ? target.path : `/${target.path ?? ""}`;
    requestedUrl = `${protocol}://localhost:${target.port}${path}`;
  }
  const resolution = resolvePreviewTarget(environmentId, requestedUrl);
  if (resolution.kind === "unreachable") {
    throw new Error(UNREACHABLE_MESSAGES[resolution.reason](resolution.environmentLabel));
  }
  return { requestedUrl, resolvedUrl: resolution.url, resolutionKind: "direct", environmentId };
}

export function resolvePreviewTarget(
  environmentId: EnvironmentId,
  rawUrl: string,
): PreviewTargetResolution {
  const parsed = parsePreviewUrl(rawUrl);
  // Malformed input keeps the normal navigation error path.
  if (!parsed) return { kind: "reachable", url: rawUrl };
  if (!isLoopbackHost(parsed.hostname)) return { kind: "reachable", url: parsed.toString() };
  // The server's own origin (e.g. an SSH-forwarded asset URL) is already reachable.
  const serverUrl = readPreparedConnection(environmentId)?.httpBaseUrl;
  if (serverUrl !== undefined && parseUrl(serverUrl)?.origin === parsed.origin) {
    return { kind: "reachable", url: parsed.toString() };
  }
  const reach = classifyEnvironmentReach(environmentId);
  if (reach.kind === "unreachable") {
    return {
      kind: "unreachable",
      reason: reach.reason,
      environmentLabel: reach.label,
      url: parsed.toString(),
    };
  }
  if (reach.kind === "same-host") {
    // A wildcard bind is not a navigable address; loopback is.
    if (isWildcardHost(parsed.hostname)) parsed.hostname = "localhost";
    // Framed in a browser tab, an HTTP page goes through the environment's
    // gateway (on the address that reaches its server), keeping the loopback a
    // direct open would load. The gateway serves no HTTPS and admits only
    // `localhost` or a loopback address; anything else opens directly.
    const host = unbracket(parsed.hostname);
    return canFrameGatewayPreviews() &&
      parsed.protocol === "http:" &&
      (host === "localhost" || isIpLiteral(host))
      ? { kind: "gateway", via: "host", host: reach.host, direct: true, url: parsed.toString() }
      : { kind: "reachable", url: parsed.toString() };
  }
  // Every loopback spelling names the same server port, so the canonical form
  // is one origin per port. The scheme is kept: the gateway refuses HTTPS itself.
  parsed.hostname = "localhost";
  const url = parsed.toString();
  return reach.kind === "host"
    ? { kind: "gateway", via: "host", host: reach.host, url }
    : { kind: "gateway", via: "ssh", url };
}
