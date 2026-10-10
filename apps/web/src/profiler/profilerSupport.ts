import { type ServerConfig, WS_METHODS } from "@bibcode/contracts";

/**
 * What an older server answers a profiler RPC with. It is a connection-level defect: it fails
 * every request in flight on that connection and silences its config stream, so the method
 * must never be sent again there (the `pairingAdd.ts` pattern).
 */
const UNSUPPORTED_DEFECTS: ReadonlySet<string> = new Set([
  `Unknown request tag: ${WS_METHODS.serverGetProfilerStatus}`,
  `Unknown request tag: ${WS_METHODS.serverReportProfilerClientBatch}`,
]);

export const PROFILER_UNSUPPORTED_MESSAGE =
  "This server's BiBCode version has no profiler. Update the server to record.";

/** Whether the server has the profiler RPCs. Only one that says so is ever sent them. */
export function serverHasProfiler(serverConfig: ServerConfig | null | undefined): boolean {
  return serverConfig?.environment.capabilities.profiler === true;
}

/**
 * Whether the server records: it has the profiler and its `enableProfiler` is on. An older
 * server stores and echoes back an `enableProfiler: true` that it never acts on.
 */
export function profilerRecording(serverConfig: ServerConfig | null | undefined): boolean {
  return serverHasProfiler(serverConfig) && serverConfig?.settings.enableProfiler === true;
}

/** Whether a squashed failure is a server refusing a profiler method it does not know. */
export function isProfilerUnsupported(failure: unknown): boolean {
  return typeof failure === "string" && UNSUPPORTED_DEFECTS.has(failure);
}

/** Environments that refused a profiler method as unknown, for the life of this page. */
const refusedEnvironments = new Set<string>();

/** Remembers that `environmentId` refused a profiler method, so it is never sent one again. */
export function rememberProfilerRefused(environmentId: string): void {
  refusedEnvironments.add(environmentId);
}

/** Whether `environmentId` refused a profiler method earlier on this page. */
export function profilerRefused(environmentId: string): boolean {
  return refusedEnvironments.has(environmentId);
}
