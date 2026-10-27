#!/usr/bin/env node
import * as NodeChildProcess from "node:child_process";
import * as NodeFS from "node:fs";
import * as NodeURL from "node:url";

// Rust test binaries carry no embedded manifest, so Windows applies UAC
// installer detection to any of them whose file name contains "install",
// "setup", "update", or "patch" (for example `remote_update_rpc-*.exe` and
// `coddy_studio_updater_verifier-*.exe`) and refuses to start them without
// elevation ("The requested operation requires elevation", surfaced by Node as
// EACCES). Declaring `asInvoker` in the sidecar manifest opts every target out
// of that heuristic alongside the Common Controls v6 dependency.
export const COMMON_CONTROLS_V6_MANIFEST = `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false" />
      </requestedPrivileges>
    </security>
  </trustInfo>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity
        type="win32"
        name="Microsoft.Windows.Common-Controls"
        version="6.0.0.0"
        processorArchitecture="*"
        publicKeyToken="6595b64144ccf1df"
        language="*"
      />
    </dependentAssembly>
  </dependency>
</assembly>
`;

export function runWindowsCargoTarget(args, options = {}) {
  const consoleError = options.consoleError ?? console.error;
  if (args.length === 0) {
    consoleError("Usage: node scripts/run-windows-cargo-target.mjs <executable> [...args]");
    return 2;
  }

  const executable = args[0];
  const manifestPath = `${executable}.manifest`;
  const writeFileSync = options.writeFileSync ?? NodeFS.writeFileSync;
  const rmSync = options.rmSync ?? NodeFS.rmSync;
  const spawnSync = options.spawnSync ?? NodeChildProcess.spawnSync;
  const utimesSync = options.utimesSync ?? NodeFS.utimesSync;
  const now = options.now ?? (() => new Date());

  writeFileSync(manifestPath, COMMON_CONTROLS_V6_MANIFEST, "utf8");
  // Windows caches the sidecar-manifest lookup per executable path and
  // modification time, so a manifest written after an earlier launch would be
  // ignored until the binary changes. Touching the executable invalidates that
  // cache; Cargo freshness is driven by source and fingerprint timestamps, not
  // by the output binary's own mtime.
  try {
    const timestamp = now();
    utimesSync(executable, timestamp, timestamp);
  } catch {}
  try {
    const result = spawnSync(executable, args.slice(1), {
      stdio: "inherit",
      shell: false,
    });
    if (result.error) {
      consoleError(
        `Failed to launch Windows Cargo target "${executable}": ${result.error.message}`,
      );
      return 1;
    }
    return result.status ?? 1;
  } finally {
    try {
      rmSync(manifestPath, { force: true });
    } catch {}
  }
}

if (
  process.argv[1] !== undefined &&
  import.meta.url === NodeURL.pathToFileURL(process.argv[1]).href
) {
  process.exit(runWindowsCargoTarget(process.argv.slice(2)));
}
