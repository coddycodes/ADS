import { REMOTE_PAIRING_CODE_VERSION, RemotePairingCodePayload } from "@coddy/contracts";
import * as Schema from "effect/Schema";

export class PairingCodeParseError extends Schema.TaggedError<PairingCodeParseError>()(
  "PairingCodeParseError",
  { detail: Schema.String },
) {
  override get message(): string {
    return `The pairing code is invalid: ${this.detail}`;
  }
}

export class PairingCodeUnsupportedVersionError extends Schema.TaggedError<PairingCodeUnsupportedVersionError>()(
  "PairingCodeUnsupportedVersionError",
  { version: Schema.Finite },
) {
  override get message(): string {
    return "This pairing code was created by a newer Coddy Studio. Update this app, then try again.";
  }
}

const decodePayload = Schema.decodeUnknownSync(RemotePairingCodePayload);

function base64UrlDecode(code: string): string {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(code, "base64url").toString("utf8");
  }
  const base64 = code.replaceAll("-", "+").replaceAll("_", "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
  return new TextDecoder().decode(bytes);
}

function base64UrlEncode(text: string): string {
  if (typeof Buffer !== "undefined") {
    return Buffer.from(text, "utf8").toString("base64url");
  }
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function encodePairingCode(payload: RemotePairingCodePayload): string {
  return base64UrlEncode(JSON.stringify(payload));
}

export function resolvePairingDeepLinkCode(rawUrl: string): string | null {
  let url: URL;
  try {
    url = new URL(rawUrl.trim());
  } catch {
    return null;
  }
  if (url.protocol !== "coddy-studio:") return null;
  const isAuthorityForm = url.hostname === "pair" && (url.pathname === "" || url.pathname === "/");
  const isPathForm = url.hostname === "" && url.pathname === "/pair";
  if (!isAuthorityForm && !isPathForm) return null;
  const code = url.searchParams.get("code")?.trim() ?? "";
  return code.length > 0 ? code : null;
}

function extractCode(raw: string): string {
  const trimmed = raw.trim();
  if (trimmed.toLowerCase().startsWith("coddy-studio:")) {
    const code = resolvePairingDeepLinkCode(trimmed);
    if (code === null) {
      throw new PairingCodeParseError({
        detail: "the URL is not a pairing link or carries no code",
      });
    }
    return code;
  }
  if (trimmed.startsWith("http://") || trimmed.startsWith("https://")) {
    let url: URL;
    try {
      url = new URL(trimmed);
    } catch (cause) {
      throw new PairingCodeParseError({ detail: `unparsable URL (${String(cause)})` });
    }
    const code = url.searchParams.get("code");
    if (code === null || code === "") {
      throw new PairingCodeParseError({ detail: "the URL carries no code parameter" });
    }
    return code;
  }
  return trimmed;
}

export function parsePairingCode(raw: string): RemotePairingCodePayload {
  const code = extractCode(raw);
  if (!/^[A-Za-z0-9_-]+$/.test(code)) {
    throw new PairingCodeParseError({ detail: "the code is not base64url" });
  }
  let parsed: unknown;
  try {
    parsed = JSON.parse(base64UrlDecode(code));
  } catch (cause) {
    throw new PairingCodeParseError({ detail: `not base64url JSON (${String(cause)})` });
  }
  const version =
    typeof parsed === "object" && parsed !== null && "v" in parsed
      ? (parsed as { v: unknown }).v
      : null;
  if (typeof version !== "number" || !Number.isInteger(version)) {
    throw new PairingCodeParseError({ detail: "the payload has no integer version" });
  }
  if (version !== REMOTE_PAIRING_CODE_VERSION) {
    throw new PairingCodeUnsupportedVersionError({ version });
  }
  try {
    return decodePayload(parsed);
  } catch (cause) {
    throw new PairingCodeParseError({ detail: `payload shape mismatch (${String(cause)})` });
  }
}

export function buildPairingDeepLink(code: string): string {
  const query = new URLSearchParams({ code });
  return `coddy-studio://pair?${query.toString()}`;
}

export function buildBrowserPairUrl(endpoint: string, code: string): string {
  const url = new URL(endpoint);
  url.pathname = "/pair";
  url.search = "";
  url.searchParams.set("code", code);
  url.hash = "";
  return url.toString();
}
