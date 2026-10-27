# Project data safety and recovery

Coddy Studio stores its project catalog in a Rust/SQLite server store, separately
from the installed application files. A normal in-place application update
replaces the application and should retain the selected store. If projects are
missing, do not assume that the old database was deleted: a different account,
home, explicit root, state kind, symlink/junction target, WSL distribution, or
remote endpoint can select another store and produce the same symptom.

Do not copy, rename, delete, or open `state.sqlite`, its WAL/SHM files, or the
`environment-id` marker while any Coddy Studio process is running. Close Coddy Studio and
its local/WSL backends first, then use the desktop recovery dialog or the
offline `coddy-studio storage inspect` command.

## What identifies a store

The base root defaults to the current user's `~/.coddy/studio`. For the server, an
explicit `--base-dir` wins over `CODDY_STUDIO_HOME`; desktop bootstrap configuration
can also supply the root. Installed desktop builds use the `userdata` state
kind and development builds use `dev`. They normally share the base root but
do not share the state directory.

The recovery dialog shows:

- **Requested root:** the configured path before filesystem aliases are
  resolved.
- **Effective root:** the canonical location after symlinks, junctions, and
  existing ancestors are resolved.
- **Storage ID:** the persistent `storageInstanceId` stored in the
  `environment-id` marker.
- **Verified backups:** the newest three verified generations for that storage
  identity and state kind. Backups are created before a migration and before a
  coordinated in-app update.

An alias warning means that the requested and effective roots differ. It is not
itself corruption, but changing the target of an alias selects different data.
Normal server descriptors publish the storage UUID and never publish either
root or alias diagnostics.

## Expected outcomes by platform

| Platform and scenario                                                                                                     | Observable outcome                                                                                                                     | Safe action                                                                                                                                                                                             |
| ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Windows native: `CODDY_STUDIO_HOME`, CLI `--base-dir`, Windows account/profile/home, drive, or junction/reparse target changed | A different effective root or store is selected; projects may look empty or a storage mismatch is blocked.                             | Open **Project data recovery**, compare requested/effective roots and Storage ID, then restore the original configuration or select a verified backup. Do not repoint aliases while Coddy Studio is running. |
| Windows native: permissions, endpoint protection, Controlled Folder Access, or another security tool blocks files         | Inspection/startup reports unavailable or recovery required; startup must not replace the store.                                       | Close Coddy Studio, restore access or allow-list the data root, retry inspection, and export redacted diagnostics if it still fails.                                                                         |
| Windows native: database or marker is missing, malformed, unrelated, or corrupt                                           | Classification fails closed. A marker-without-database state is never treated as first run.                                            | Inspect offline, restore a verified backup if available, or explicitly **Start empty** after accepting preservation of the current files.                                                               |
| Windows WSL: distro, Linux user/home, `CODDY_STUDIO_HOME`, or selected root changed                                            | The desktop reaches a different Linux root or cannot resolve the configured plan; storage mismatch or unavailable state is shown.      | Re-select the intended distro/root and retry. Recovery uses the exact root recorded in the launch plan and never guesses another WSL home.                                                              |
| Windows WSL-only distro unavailable                                                                                       | A tagged WSL-primary error is shown; native Windows is not launched automatically.                                                     | Restore the distro, select another distro, or explicitly **Switch to Windows**. Switching is a deliberate topology change, not recovery of the WSL store.                                               |
| Windows native plus optional WSL secondary unavailable                                                                    | Native primary stays live; the stable WSL environment remains desired/unavailable and cached rows remain visible.                      | Retry WSL, choose a distro, or explicitly turn off/replace that secondary.                                                                                                                              |
| macOS: account/home, `CODDY_STUDIO_HOME`, or symlink target changed                                                            | Another effective root is selected and may appear empty or trigger a storage mismatch.                                                 | Compare requested/effective roots and Storage ID; restore the original account/root/target or use verified recovery.                                                                                    |
| macOS: permissions, quarantine, endpoint security, backup/cleanup software, or another security tool interferes           | The store or application may be unavailable; classification/recovery fails closed instead of silently creating over existing evidence. | Close Coddy Studio, resolve OS access/quarantine policy, retry inspection, and preserve/export diagnostics before changing files.                                                                            |
| macOS: database or marker missing/corrupt                                                                                 | Recovery-required state; no implicit replacement of an existing artifact.                                                              | Restore a verified backup or explicitly start empty after preservation.                                                                                                                                 |
| Linux: account/home, `CODDY_STUDIO_HOME`, symlink, mount, or mount target changed                                              | A different or unavailable effective root is selected. Moving the AppImage alone should not move the default root.                     | Restore the original root/mount/alias target and inspect; do not infer the store location from the AppImage location.                                                                                   |
| Linux: AppImage replaced manually or updated by an external package manager                                               | The in-app update coordinator did not run. Project data should remain separate, but no automatic `pre-update` backup was requested.    | Close all Coddy Studio processes before replacement. Afterward compare root and Storage ID; use recovery if classification fails.                                                                            |
| Linux: permissions or database/marker corruption                                                                          | Startup is unavailable or recovery required and fails closed.                                                                          | Fix ownership/permissions without deleting files, then inspect; restore a verified backup or explicitly start empty.                                                                                    |
| Any platform: a saved bearer, relay, or SSH endpoint now reaches a different store                                        | The same logical environment reports a different non-null storage UUID and is blocked before session synchronization.                  | Verify the endpoint, then reconnect to the intended server or explicitly adopt the new Storage ID. Adoption does not merge databases or clear the old server.                                           |
| Any platform: the client connection catalog is corrupt                                                                    | Catalog health becomes `recovery-required`; mutations and an authoritative empty-project result are blocked.                           | Preserve diagnostics and use the separate explicit catalog-reset support action. It does not delete server project databases, and corrupt bytes are never silently rewritten as empty.                  |
| Any platform: migration or coordinated update backup cannot be verified                                                   | Migration/update stops before it modifies or installs against the unprotected store.                                                   | Resolve the reported path, space, permission, or filesystem problem, then retry. Never bypass the primary-store protection dialog.                                                                      |
| Any platform: explicit **Start empty**                                                                                    | Existing database, WAL/SHM, and marker files are first preserved in a private recovery generation; the next store gets a new UUID.     | Confirm separately, restart, review the new Storage ID, and explicitly adopt it only if the empty store is intended.                                                                                    |

## Recovery workflow

1. Close every Coddy Studio window and verify that no native or WSL Coddy Studio process
   owns the selected root. Do not stop a process by deleting its files.
2. Open **Project data recovery** from the affected local environment. The
   dialog is available only for desktop-owned native and WSL backends; remote
   bearer/relay/SSH stores must be recovered on their owning machine. When a
   desktop backend cannot start because Rust classifies its store as
   recovery-required, the host retains the stopped launch target and the
   dialog opens automatically even if the failure happened before the WebView
   mounted. The renderer re-inspects the host status; it does not infer this
   state from a generic connection error.
3. Compare requested root, effective root, and Storage ID with the environment
   you intended to open, and confirm whether you launched an installed
   (`userdata`) or development (`dev`) build. Use **Open data folder** only
   after the target is stopped. **Export diagnostics** produces bounded
   redacted evidence rather than database contents or credentials.
4. Prefer **Restore selected backup** when a verified generation contains the
   intended store. Restore verifies identity, location, checksum, SQLite
   integrity, and migration history, preserves the current live files, then
   installs the selected generation under the storage-operation lock.
5. Use **Start empty** only when you deliberately want a new store. The current
   database, sidecars, and marker are preserved first; they are not deleted.
   The new UUID still requires explicit client adoption before normal retry.
   **Use this data location** is a separate adoption action and always presents
   the desktop-native confirmation that the two locations are not merged.
   Cancelling that confirmation leaves the accepted identity unchanged.

The project list is intentionally conservative during this process. Cached
projects remain visible through reconnect, degradation, unavailability,
storage mismatch, and recovery. **No projects yet** is valid only after the
environment catalog has loaded, at least one environment is desired, and every
desired environment has supplied a successful live authoritative snapshot that
is genuinely empty.

## Limits and unrelated leftovers

The first release that creates and checks storage UUIDs cannot detect a switch
that happened earlier between two otherwise valid unmarked Coddy Studio databases.
Protection begins when a database receives an `environment-id` UUID and the
client records it. Preserve any older roots until the intended store has been
identified and backed up.

Coddy Studio has no T4Code compatibility, migration, scanning, or automatic
adoption. T4Code leftovers are inert unless `CODDY_STUDIO_HOME`, `--base-dir`, a
desktop launch plan, or a remote endpoint explicitly points current Coddy Studio at
them. If they overlap the selected current root, the current marker/database
classifier applies and fails closed; Coddy Studio does not reinterpret them as a
legacy store.
