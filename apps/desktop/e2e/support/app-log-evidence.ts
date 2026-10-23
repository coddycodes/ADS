// @effect-diagnostics nodeBuiltinImport:off - Packaged UI evidence copies the app's own logs.
import * as NodeFS from "node:fs";
import * as NodePath from "node:path";

/** The most of each app log kept as evidence: the end of the run, where a failure is. */
export const APP_LOG_EVIDENCE_MAX_BYTES = 4 * 1024 * 1024;

/**
 * Copies the tail of the packaged app's logs (`<state root>/userdata/logs/*.log`) into the
 * artifact directory as `app-<name>`, so a failed run keeps the native page-load lifecycle,
 * webview process failures and server errors next to its screenshots. Best effort: evidence
 * collection never fails the run.
 */
export function copyAppLogEvidence(
  stateRoot: string,
  artifactDirectory: string,
  maxBytes: number = APP_LOG_EVIDENCE_MAX_BYTES,
): ReadonlyArray<string> {
  const logDirectory = NodePath.join(stateRoot, "userdata", "logs");
  let names: ReadonlyArray<string>;
  try {
    names = NodeFS.readdirSync(logDirectory).filter((name) => name.endsWith(".log"));
  } catch {
    return [];
  }
  const copied: Array<string> = [];
  for (const name of names) {
    try {
      const source = NodePath.join(logDirectory, name);
      const size = NodeFS.statSync(source).size;
      const length = Math.min(size, maxBytes);
      const buffer = Buffer.alloc(length);
      const descriptor = NodeFS.openSync(source, "r");
      try {
        NodeFS.readSync(descriptor, buffer, 0, length, size - length);
      } finally {
        NodeFS.closeSync(descriptor);
      }
      const target = NodePath.join(artifactDirectory, `app-${name}`);
      NodeFS.mkdirSync(artifactDirectory, { recursive: true });
      NodeFS.writeFileSync(target, buffer);
      copied.push(target);
    } catch {
      // A log rotated away or still locked is skipped; the rest are kept.
    }
  }
  return copied;
}
