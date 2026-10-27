import type {
  AdvertisedEndpoint,
  AdvertisedEndpointHostedHttpsCompatibility,
  AdvertisedEndpointProvider,
  AdvertisedEndpointReachability,
  AdvertisedEndpointSource,
  AdvertisedEndpointStatus,
} from "@coddy/contracts";

export interface CreateAdvertisedEndpointInput {
  readonly id: string;
  readonly label: string;
  readonly provider: AdvertisedEndpointProvider;
  readonly httpBaseUrl: string;
  readonly reachability: AdvertisedEndpointReachability;
  readonly hostedHttpsCompatibility?: AdvertisedEndpointHostedHttpsCompatibility;
  readonly desktopCompatibility?: "compatible" | "unknown";
  readonly source: AdvertisedEndpointSource;
  readonly status?: AdvertisedEndpointStatus;
  readonly isDefault?: boolean;
  readonly description?: string;
}

export function normalizeHttpBaseUrl(rawValue: string): string {
  const url = new URL(rawValue);
  if (url.protocol === "ws:") {
    url.protocol = "http:";
  } else if (url.protocol === "wss:") {
    url.protocol = "https:";
  }

  if (url.protocol !== "http:" && url.protocol !== "https:") {
    throw new Error(`Endpoint must use HTTP or HTTPS. Received ${url.protocol}`);
  }

  url.pathname = "/";
  url.search = "";
  url.hash = "";
  return url.toString();
}

export function deriveWsBaseUrl(httpBaseUrl: string): string {
  const url = new URL(normalizeHttpBaseUrl(httpBaseUrl));
  url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
  return url.toString();
}

export function classifyHostedHttpsCompatibility(
  httpBaseUrl: string,
  fallback: AdvertisedEndpointHostedHttpsCompatibility = "unknown",
): AdvertisedEndpointHostedHttpsCompatibility {
  const url = new URL(normalizeHttpBaseUrl(httpBaseUrl));
  if (url.protocol === "http:") {
    return "mixed-content-blocked";
  }
  return fallback === "mixed-content-blocked" ? "unknown" : fallback;
}

export type PairingEndpointClassification =
  | "loopback"
  | "private-network"
  | "public"
  | "unconnectable";

function parseIpv4(host: string): readonly [number, number, number, number] | null {
  const segments = host.split(".");
  if (segments.length !== 4 || segments.some((segment) => !/^\d+$/u.test(segment))) return null;
  const octets = segments.map(Number);
  if (octets.some((octet) => !Number.isInteger(octet) || octet < 0 || octet > 255)) return null;
  return octets as [number, number, number, number];
}

function isPrivateIpv4(octets: readonly [number, number, number, number]): boolean {
  const [a, b] = octets;
  return (
    a === 10 ||
    (a === 172 && b >= 16 && b <= 31) ||
    (a === 192 && b === 168) ||
    (a === 100 && b >= 64 && b <= 127) ||
    (a === 169 && b === 254)
  );
}

function parseIpv4MappedIpv6(host: string): readonly [number, number, number, number] | null {
  const match = /^(?:::ffff:|0:0:0:0:0:ffff:)([0-9a-f]{1,4}):([0-9a-f]{1,4})$/u.exec(host);
  if (match === null) return null;
  const high = Number.parseInt(match[1]!, 16);
  const low = Number.parseInt(match[2]!, 16);
  return [high >>> 8, high & 0xff, low >>> 8, low & 0xff];
}

function isPrivateIpv6(host: string): boolean {
  if (!host.includes(":")) return false;
  const firstSegment = host.split(":", 1)[0] ?? "";
  if (!/^[0-9a-f]{1,4}$/u.test(firstSegment)) return false;
  const first = Number.parseInt(firstSegment, 16);
  return (first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80;
}

export function classifyPairingEndpoint(endpoint: string): PairingEndpointClassification {
  let url: URL;
  try {
    url = new URL(normalizeHttpBaseUrl(endpoint));
  } catch {
    return "unconnectable";
  }
  if (url.port === "0") {
    return "unconnectable";
  }
  const host = url.hostname.replace(/^\[|\]$/g, "").toLowerCase();
  if (host === "0.0.0.0" || host === "::" || host === "") {
    return "unconnectable";
  }
  const ipv4 = parseIpv4(host);
  const mappedIpv4 = parseIpv4MappedIpv6(host);
  const effectiveIpv4 = ipv4 ?? mappedIpv4;
  if (mappedIpv4?.every((octet) => octet === 0)) {
    return "unconnectable";
  }
  if (host === "localhost" || host === "::1" || effectiveIpv4?.[0] === 127) {
    return "loopback";
  }
  if (effectiveIpv4 !== null && isPrivateIpv4(effectiveIpv4)) {
    return "private-network";
  }
  if (isPrivateIpv6(host)) {
    return "private-network";
  }
  return "public";
}

export function createAdvertisedEndpoint(input: CreateAdvertisedEndpointInput): AdvertisedEndpoint {
  const httpBaseUrl = normalizeHttpBaseUrl(input.httpBaseUrl);
  return {
    id: input.id,
    label: input.label,
    provider: input.provider,
    httpBaseUrl,
    wsBaseUrl: deriveWsBaseUrl(httpBaseUrl),
    reachability: input.reachability,
    compatibility: {
      hostedHttpsApp:
        input.hostedHttpsCompatibility ?? classifyHostedHttpsCompatibility(httpBaseUrl),
      desktopApp: input.desktopCompatibility ?? "compatible",
    },
    source: input.source,
    status: input.status ?? "available",
    ...(input.isDefault === undefined ? {} : { isDefault: input.isDefault }),
    ...(input.description === undefined ? {} : { description: input.description }),
  };
}
