import {
  MIN_COMPATIBLE_REMOTE_PROTOCOL,
  REMOTE_PROTOCOL_VERSION,
  type ExecutionEnvironmentDescriptor,
} from "@coddy/contracts";

/**
 * Compatibility verdict for one environment, computed from the remote protocol
 * window the server advertises on its environment descriptor.
 *
 * - `legacy`: the server predates the window (both fields decode-defaulted to
 *   0). Rendered as "Limited compatibility"; the existing default-false
 *   capability booleans continue to govern behavior.
 * - `server-too-old` / `client-too-old`: one side is outside the two-way
 *   window and the pairing cannot operate.
 */
export type CompatVerdict =
  | { kind: "compatible" }
  | { kind: "legacy" }
  | { kind: "server-too-old"; serverVersion: number; minSupported: number }
  | { kind: "client-too-old"; serverMinCompatible: number; clientVersion: number };

/**
 * Two-way window rule: compatible iff the server's version meets this client's
 * floor and this client's version meets the server's floor. Evaluation order
 * is normative: legacy (both fields 0), then server-too-old, then
 * client-too-old.
 *
 * Failed descriptor probes carry no cache of their own: retry pacing is the
 * supervisor's existing 1/2/4/8/16 s reconnection backoff.
 */
export function computeCompatVerdict(
  descriptor: Pick<
    ExecutionEnvironmentDescriptor,
    "remoteProtocolVersion" | "minCompatibleRemoteProtocol"
  >,
): CompatVerdict {
  const serverVersion = descriptor.remoteProtocolVersion;
  const serverMinCompatible = descriptor.minCompatibleRemoteProtocol;
  if (serverVersion === 0 && serverMinCompatible === 0) {
    return { kind: "legacy" };
  }
  if (serverVersion < MIN_COMPATIBLE_REMOTE_PROTOCOL) {
    return {
      kind: "server-too-old",
      serverVersion,
      minSupported: MIN_COMPATIBLE_REMOTE_PROTOCOL,
    };
  }
  if (REMOTE_PROTOCOL_VERSION < serverMinCompatible) {
    return {
      kind: "client-too-old",
      serverMinCompatible,
      clientVersion: REMOTE_PROTOCOL_VERSION,
    };
  }
  return { kind: "compatible" };
}
