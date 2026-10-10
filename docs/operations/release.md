# Release Checklist

This document describes the Tauri 2 desktop and native standalone-server release
workflow. The repository does not package or publish Electron artifacts.

## Release Workflow

`.github/workflows/release.yml` supports:

- stable releases from tags matching `v*.*.*`; and
- manual stable or nightly releases through `workflow_dispatch`.

For validation-only release-candidate verification from a feature branch, dispatch the workflow
with `validate_only=true`, `publish=false`, and a unique prerelease version such
as `0.4.2-validation.123`. This path runs the complete native desktop and server
matrices, assembles and checksums the public asset set, and uploads it as the
`validated-release-assets` Actions artifact. It does not create or modify a Git
tag or GitHub Release. It runs `vp check` and `vp run typecheck` before the
native matrices and relies on the pull-request CI workflow for the full test
graph. `validate_only` and `publish` are mutually exclusive.

Publication-capable preflight runs `vp check`, `vp run typecheck`, and
`vp run -r --concurrency-limit 1 test`; validation-only preflight skips the
duplicated full test graph after check and typecheck. The test graph runs one
package task at a time so each Cargo invocation finishes compiling before its
test binaries run: with the default concurrency the server suites ran while
`rustc` was still building the desktop crate, and tests that hold 2-second
deadlines (hook body reads, port-release probes) failed on starved hosted
runners. The direct recursive `-r` invocation applies the limit to package
tasks; a limit on the root `test` wrapper does not constrain its nested graph.
The publication preflight has a 60-minute budget because a cold
runner compiles the full Rust workspace through the package graph; measured
hosted runs exceeded both 30 and 45 minutes without a failing test. The build
matrix then creates native Tauri installers on the matching operating system:

| Platform | Runner                  | Architecture | Installer       |
| -------- | ----------------------- | ------------ | --------------- |
| macOS    | `macos-26`              | arm64        | DMG             |
| macOS    | `macos-15-intel`        | x64          | DMG             |
| Linux    | `ubuntu-22.04-arm`      | arm64        | AppImage        |
| Linux    | `ubuntu-22.04`          | x64          | AppImage        |
| Windows  | `windows-11-vs2026-arm` | arm64        | NSIS executable |
| Windows  | `windows-2025`          | x64          | NSIS executable |

The native Intel release producers use supported macOS 15 for both desktop and
server packaging. macOS 26 remains the ARM producer and the Intel runtime-test
environment in main CI, packaged UI, and upgrade validation. Both release
producer hosts still require the exact mounted DMG's recursive ad-hoc signature
and the same Finder pixel check before upload. A macOS 15 Intel pass does not
qualify macOS 26 Intel Finder rendering; that hosted-runner scenario remains
separate. The producer selection does not alter the configured minimum macOS
version, Rust/updater/archive identities, platform count, or signing policy.

Release builds retain `panic=unwind`. Wry's macOS custom-protocol handlers use
Objective-C exception recovery when navigation cancels an in-flight request;
`panic=abort` turns that recoverable condition into process termination. Thin LTO,
one codegen unit, and symbol stripping remain enabled; the release jobs strip
their binaries after dumping symbols (see [Symbol assets](#symbol-assets)).
Native macOS CI executes the release-profile `objc_exception_probe`, and the
desktop crate rejects an abort profile on macOS.

Each matrix job installs the frontend build toolchain, Rust, and the pinned
`dump_syms`, restores Cargo caches, and runs
`scripts/build-desktop-artifact.ts --symbols`. Tauri compiles the native host
and in-process server and embeds the built React assets. No Node runtime or
TypeScript server is packaged.

A separate native matrix builds `bibcode` plus the matching static web client for the
same six targets. macOS and Linux receive `.tar.gz` archives; Windows receives `.zip`.
Linux also receives direct-download `.deb` and `.rpm` packages for both architectures.
The Linux server binary is compiled on an Ubuntu 20.04 compatibility baseline, then its
packages are installed and exercised on Ubuntu 22.04, Ubuntu 24.04, Debian 12, Rocky
Linux 9, and Fedora 44.

Numeric stable versions are updater candidates and are marked latest only after
manual publication approval. Stable prerelease versions and manual nightly
releases are GitHub prereleases, are never marked latest, and publish desktop
installers plus standalone server distributions without updater metadata.

## Supported Platforms

- macOS 11 or newer on Apple Silicon (`arm64`) and Intel (`x64`);
- Windows 10 or 11 on `x64`, and Windows 11 on ARM64;
- Linux x64 and ARM64 AppImages built on matching Ubuntu 22.04 runners and exercised on Ubuntu 22.04,
  Ubuntu 24.04, and Debian 12.

`scripts/run-msvc.mjs` selects the requested native MSVC architecture. Linux desktop
release artifacts use Ubuntu 22.04; standalone server builds use the older compatibility
baseline described above.

## Standalone Server Assets

Each release contains `bibcode-server-v<version>-<os>-<architecture>.tar.gz` for macOS
and Linux or `.zip` for Windows. Public OS/architecture keys are
`darwin-{aarch64,x86_64}`, `linux-{aarch64,x86_64}`, and
`windows-{aarch64,x86_64}`. Linux also publishes:

- `bibcode-server_<version>_arm64.deb`
- `bibcode-server_<version>_amd64.deb`
- `bibcode-server-<version>-1.aarch64.rpm`
- `bibcode-server-<version>-1.x86_64.rpm`

The packages are direct GitHub Release downloads, not hosted APT or RPM repositories.
Before staging, each server job dumps the binary's symbols and strips it; see
[Symbol assets](#symbol-assets).
Every release includes `bibcode-server-SHA256SUMS`; optional server `.minisig` files
appear only when both dedicated server-signing secrets are configured.
Windows server archives are written through `scripts/create-portable-zip.ps1`, which assigns
forward-slash entry names explicitly. The artifact builder rejects a mislabeled non-ZIP payload,
backslash entry names, duplicate entries, traversal, or paths outside the versioned root.

## Symbol Assets

Each release also contains twelve Breakpad symbol archives, one for each
release binary: `bibcode-desktop-symbols-<version>-<os>-<architecture>.zip` and
`bibcode-server-symbols-<version>-<os>-<architecture>.zip`, with the public
OS/architecture keys of the server archives. Each archive holds one `.sym` file
in symbol-store layout, `<module>/<debug id>/<module>.sym`. On Windows the module
is named after its PDB: `<name>.pdb/<debug id>/<name>.sym`. `minidump-stackwalk`
and the profiler's symbolication read that layout from the extracted archive.
`scripts/assemble-release-assets.ts` requires all twelve and still rejects any
other unlisted file. The archives are not covered by `bibcode-server-SHA256SUMS`
or the optional server signatures.

The shipped binaries stay stripped. `[profile.release]` keeps `strip = "symbols"`,
and the release jobs override the profile through the environment of their own
builds, so `dump_syms` reads the symbols before they are removed:

- `CARGO_PROFILE_RELEASE_STRIP=none` and
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` on every target;
- `CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed` on macOS, so rustc writes a dSYM;
- `RUSTFLAGS=-C link-arg=-Wl,--build-id=sha1` on Linux, so the debug ID that
  minidumps and symbols share never depends on the linker's default.

The desktop jobs run `scripts/build-desktop-artifact.ts --symbols`:
`tauri build --no-bundle`, then `scripts/dump-symbols.ts`, then the strip, then
`tauri bundle`. The Tauri CLI's bundle step does not run Cargo, and must not:
Cargo re-links its unstripped `deps/` artifact over the stripped binary even
when nothing needs rebuilding. The wrapper therefore fails the job unless the
binary is still the stripped file after bundling, and unless the copy in each
bundle that has a readable one (the macOS app, the AppImage's file system) has
its build ID and symbol table; Windows installers pack the binary itself. The
server jobs build `bibcode` with the same profile (the Linux build inside its
Ubuntu 20.04 container), then dump and strip it on the runner before staging.
Both strip as rustc does for executables, with `strip --strip-all` on Linux and
plain `strip` on macOS, into a copy that replaces the binary, so Cargo's
hard-linked `deps/` artifact keeps its symbols for the next build. Apple silicon
runs only signed code, so the server job runs `codesign --verify` on the
stripped arm64 binary. Windows ships its binaries as linked and keeps the names
in the PDB, which is not published. Both matrices install `dump_syms` 2.3.9 with
`cargo install --locked`; Rust Cache keeps `~/.cargo/bin`, so a restored runner
skips that compile. `scripts/dump-symbols.ts` fails the job when a `.sym` lacks
functions with source lines or `STACK` records (unwind data). The CI
`crash-smoke` job exercises the same profile and dump on Linux, macOS, and
Windows; see [CI quality gates](./ci.md).

## Version Source

`apps/desktop/package.json` is the desktop version source.
`apps/desktop/src-tauri/tauri.conf.json` reads that version by path. The release
workflow aligns versioned application packages before building. After a
successful stable release, the finalize job updates the versioned package files
on `main` when branch protection permits the workflow token to push.

## Cloud Configuration

BiBCode Connect public configuration is optional for this fork. When Cloudflare and
Clerk production configuration exists, the workflow resolves and injects:

- `BIBCODE_CLERK_PUBLISHABLE_KEY`;
- `BIBCODE_CLERK_JWT_TEMPLATE`;
- `BIBCODE_CLERK_CLI_OAUTH_CLIENT_ID`;
- `BIBCODE_RELAY_URL`.

`BIBCODE_CLERK_CLI_OAUTH_CLIENT_ID` remains in build and release plumbing, but
the current native runtime has no matching headless Connect CLI or OAuth
consumer. It does not enable a CLI login flow.

Without that configuration, desktop artifacts are still built with BiBCode Connect
disabled. Never place `CLERK_SECRET_KEY` in client build variables or artifacts.

Relay deployment and hosted web deployment are separate from this fork's
desktop release. The workflow intentionally does not publish the upstream
`bibcode` npm package or deploy the upstream Vercel project.

## Signing And OS Trust

macOS bundles merge `apps/desktop/src-tauri/Info.plist`, which relaxes App
Transport Security for web content only so the desktop can reach plain-HTTP
remote servers on a LAN or tailnet, and declares the Local Network usage
description. macOS application bundles are signed with Tauri's ad-hoc `-` identity. This
seals the complete bundle so Gatekeeper can verify that it is intact, but it
does not associate the app with an Apple Developer team or notarize it. Users
must approve a browser-downloaded build through Settings > Privacy & Security.
Release CI mounts both macOS DMGs and verifies their recursive bundle
signatures and Finder-rendered application icons before upload. The existing
icon verifier reads the application from that exact read-only DMG mount; its
failure stops the native build job before assets can be uploaded.
The closed `mac-icon-observation` stderr records distinguish Swift entry,
workspace/bitmap/context/drawing/pixel boundaries, and workflow cleanup return. The scan
records retain capped bitmap dimensions and pixel counts, never application
paths or native errors. Diagnostic caps never change the scan or verdict.
These are observations, not alternate success criteria; missing records
remain unobserved and the mandatory Finder verdict and job budget are unchanged.
The verifier draws the workspace-provided icon at 256 points into a fixed
1024-square RGBA bitmap instead of serializing all image representations to
TIFF. Native macOS jobs run its `--self-test` raster and threshold fixtures
before building, then require the separate mounted-application verdict after
signature verification. The alpha, luminance, dark-area, and pale-area limits
remain the same; synthetic fixtures cannot qualify a release payload.

Windows artifacts remain without Authenticode. macOS remains ad-hoc
signed/unnotarized by decision (2026-09-18): an ad-hoc identity changes with
every build, so macOS privacy grants (folder access, local network) are
re-requested after each update. Only a Developer ID certificate with
notarization would make those grants persist; adopting one requires an Apple
Developer account and new signing secrets in the release workflow. Tauri updater signatures verify update payloads; they do
not replace Apple Developer ID signing, macOS notarization, or Windows
Authenticode.

Every server artifact is covered by `bibcode-server-SHA256SUMS`. If both
`BIBCODE_SERVER_SIGNING_PRIVATE_KEY` and `BIBCODE_SERVER_SIGNING_PUBLIC_KEY` are
configured, release assembly also creates and verifies `.minisig` files. Server signing
is optional and uses a key separate from the mandatory Tauri updater key.

Keep a password-protected backup of the updater private key in an approved
offline recovery location, with access restricted to release maintainers. Keep
its passphrase in a separate approved secret store.
Release CI receives the key only through the GitHub secrets
`TAURI_SIGNING_PRIVATE_KEY` and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`; never add
the key or passphrase to the repository, a release asset, or a log.

> **Recovery warning:** losing both the updater private key and its separate
> passphrase breaks the trusted update chain. Existing installations cannot
> trust payloads signed by a replacement key.

## Stable Updater

Numeric, non-prerelease stable release builds apply
`apps/desktop/src-tauri/tauri.release.conf.json` as a release-only overlay. The
base configuration intentionally has no updater endpoint or public key, so
development, E2E, stable prerelease, and ordinary local builds never perform
updater I/O. The workflow passes `--updater` only for updater candidates.

The stable updater endpoint is:

`https://github.com/mubeda/BibCode/releases/latest/download/latest.json`

Every stable `latest.json` has exactly these signed manifest targets:

- `darwin-aarch64`
- `darwin-x86_64`
- `linux-aarch64`
- `linux-x86_64`
- `windows-aarch64`
- `windows-x86_64`

The payload URLs inside the manifest must be tag-specific HTTPS GitHub Release
URLs, not the moving `latest/download` endpoint. The moving endpoint is used
only to fetch `latest.json`. Before creating a draft, release CI verifies every
manifest payload with the public key from the release overlay and requires each
manifest signature to match its adjacent `.sig` asset.

Stable-channel and nightly prereleases never receive the updater signing overlay,
updater signatures, descriptors, or `latest.json`, and never feed the app updater. They
still publish the validated standalone server matrix and checksums.

### Update installation safety

The signed stable updater uses the same project-store protection protocol on
macOS, Windows, and Linux. Before the platform installer runs, the desktop host
protects the native primary and every included running secondary with a
verified `PreUpdate` backup, then waits for those backends to stop. Windows WSL
primary and secondary runtimes participate through their authenticated desktop
bootstrap transport. A configured secondary that is not running is shown as
unprotected; the user must name that exact secondary to exclude it. The primary
is never excludable.

If preparation, cancellation, commit, backend stop, or platform installation
fails, the host attempts to restore the exact set of backends that was running
before protection began. The installer is never called while an included
backend remains uncommitted or running. This guarantee applies to updates
installed from the in-app signed stable channel. Replacing the application
manually with a DMG, NSIS executable, AppImage, or an external package manager
does not pass through the in-app coordinator; close BiBCode before performing a
manual replacement.

The coordinated sequence rejects new mutations, drains admitted mutations,
quiesces background writers, checkpoints the WAL, publishes and reloads a
verified `pre-update` backup, stops the captured backend topology, and only then
invokes the platform installer. Windows waits for the packaged process to exit
before NSIS replaces files. macOS and Linux install the candidate and relaunch
the packaged application through their platform updater flow. A failed
prepare, backup, stop, install, or relaunch leaves the verified backup and any
recovery-preservation artifacts in the data root and attempts to restart the
exact prior backend set.

The primary backend is always protected and cannot be excluded. Each running
secondary is protected independently. A configured but unavailable secondary
is shown by its exact environment ID and may be explicitly excluded; a generic
"continue anyway" is not accepted. WSL-only intent never falls back to native
Windows during update preparation or recovery.

Linux AppImage updates launched through BiBCode use this coordinator. Replacing
an AppImage directly, installing through an external package manager, or
manually replacing an NSIS/DMG installation is outside the coordinator. Close
every BiBCode backend before those operations. Application files and project
data are separate: a normal in-place updater replaces application files and
must retain the selected project-data root.

### Seeded packaged-upgrade matrix

[`desktop-upgrade-smoke.yml`](../../.github/workflows/desktop-upgrade-smoke.yml)
runs real packaged previous-stable-to-candidate and protected-baseline upgrades
for Linux ARM64/x64 AppImages, Windows ARM64/x64 NSIS installers, and macOS
ARM64/x64 DMGs.
The protected lane verifies the same storage UUID, seeded project, and a
verified `pre-update` backup after restart. A separate Windows job exercises a
WSL primary when the runner declares WSL plus an installed distribution; an
unavailable capability produces an explicit skip reason rather than emulated
coverage.

Each package is built from a disposable source checkout: the candidate and
protected baseline use the tested commit, and the previous baseline uses its
release tag. Before building, the maintained release-version helper pins the
candidate to the requested candidate version and both baselines to the previous
version. The harness verifies the package manifests, both Rust manifests,
their Cargo lock entries, and the Tauri overlay agree, so native app and embedded
server versions describe the same build. The calling checkout is not rewritten.

The three packages compile sequentially into separate Cargo output directories
under the isolated run root; the workflow's cached repository `target` does not
warm them. The macOS Intel packaging child has a 90-minute bound; every other
target retains 45 minutes. Each checkout's frozen dependency install retains
10 minutes. The complete Intel job allows 360 minutes: 270 for packaging, 30
for those installs, and 60 for setup, all upgrade lanes, evidence, and cleanup.
The other five native rows and the separate WSL job retain 240 minutes.
The `remote-install` lane reuses the protected package rather than building a
fourth package. These are build/job limits, not expected durations or changes
to product, WebDriver, or restart deadlines.

Windows installed-version observation retains a ten-second PowerShell child
bound. A timed-out child must be observed closed, including its pipes, before
the timeout can be recorded as an unavailable sample within the unchanged
overall restart deadline. The shared command owner tracks `close` from spawn
admission; cleanup that reaches its five-second bound without that proof fails
closed. Successful installation still requires the exact candidate version,
readable checksum, no candidate installer, and the later native runtime and
retained-data checks.

The harness uses an isolated root outside the checkout, an ephemeral Tauri
updater key, a loopback-only mock updater, the packaged app's embedded
WebDriver, and bounded redacted evidence. It never opens or copies the SQLite
database directly. Linux additionally requires the normal Tauri/AppImage
libraries plus Xvfb. The harness is **CI-only** and rejects local invocation before
starting an application. Never run it on a machine with a user's BiBCode app:
legacy lane cleanup can select the process name. Use the workflow's disposable
native runners and ephemeral signing keys, not production signing credentials.

The ordinary native matrix also runs `remote-install`, reusing the current-source
protected baseline in a separate data/workspace root. It generates an **Another
device** grant through the packaged UI and redeems it for a distinct test client.
If widening is unavailable, evidence records `widened: false`; source/native
transport tests then supply that leg, rather than claiming a wide live pass.
The Node driver first checks the authenticated host's updater and requires the
requested candidate to be available before starting the product coordinator.
An already-running check is followed through status reads within one 30-second
deadline; an unavailable, missing, mismatched, or failed candidate stops the lane
before installation. This matches the UI's available-update prerequisite and
ensures a fast local update cannot restart before the coordinator learns its
target. The coordinator runs over authenticated loopback RPC; when widened, it
waits for a candidate boot reachable through a local interface.
A test-only metadata observer records brief percentages/stages without changing
product coordinator deadlines.
Before installation, the remote lane creates a dedicated witness directory in
its private workspace and mints an ordinary signed upload through authenticated
RPC. It exclusively writes and flushes the private capability receipt before
starting HTTP. A real `100 Continue` admits the two-byte upload; the final byte
is held until the product coordinator reports `waiting-for-mutations`. The
helper's 20-second hold bound starts at admission. Expiry releases the harmless
body but fails qualification; it never supplies fabricated progress or changes
updater polling and deadlines. Once installation is dispatched, fixture failure
still joins the update coordinator and the HTTP request before reporting failure.
Success requires the completed upload's exact bytes and no partial file.
Fallback residue removal runs only after the lane's application cleanup has
joined; a failed stop preserves the directory.
The remote lane records an install attempt only after its authenticated
`updater.install` request is dispatched. A refused check or failed dispatch
leaves that marker false, so a failed WebDriver phase cannot be mistaken for an
installer handoff. Marker-write failures do not change the dispatched update.

Evidence requires a new boot on the candidate, unchanged storage identity,
observed download/protection progress, a verified pre-update backup, and exactly
one requester log line. Linux additionally requires one lane-specific AppImage
mount/runtime and cleans only processes still carrying that lane's exact
`BIBCODE_HOME`. Host evidence is captured before WebDriver teardown; backup and
project retention are read through public bridge/RPC observations. Credentials
stay in a private receipt outside retained evidence and are redacted from logs.
The upload receipt is also private and immutable. Evidence redaction removes
full, truncated and escaped transfer-capability labels before applying size
bounds. An invalid private receipt prevents evidence retention; only fixed
upload-witness booleans and bounds are added to the public result.
Present `null` receipts are invalid. Once remote WebDriver launch is attempted,
both phase logs and final evidence require the bootstrap receipt, even if
installation was never dispatched; earlier local preparation failures do not.
If the remote coordinator does not succeed, the phase log retains only its
typed phase/failure kind, unique phase history, and bounded valid version
strings. Host error messages and credential or identity details are omitted.
The WSL-specific matrix retains its existing protected lane; it does not repeat
the native remote-install scenario. First real remote-install execution is CI,
separate from helper/unit/syntax checks.

## Maintainer branch flow

Maintainer integrations are local `--no-ff` merges into `main`, followed by a
push of `main` and synchronization of `develop`. They do not require a pull
request. Use the checkout that owns each branch; inspect `git worktree list`
instead of assuming a checkout location. Preserve unrelated changes.

Before the release merge, compare content. The long-lived working branch does
not receive `main`'s release merge commits back, so an ancestry check alone can
reject a branch that already contains all of `main`'s content:

```sh
work_branch='<working-branch>'
git fetch origin
git diff --quiet "$(git merge-base origin/main "$work_branch")" origin/main
```

Exit zero proves `main` adds no content relative to the merge base. A nonzero
result needs inspection and reconciliation before merging; do not assume it
is only merge history. After reconciliation, rerun candidate verification.

Prepare the intended version and curated `CHANGELOG.md` entry on the working
branch. The version updater owns both application Cargo manifests and
`Cargo.lock` as well as the four package manifests:

```sh
version='<numeric-stable-version>'
node scripts/update-release-package-versions.ts "$version"
vp fmt apps/server/package.json apps/desktop/package.json apps/web/package.json packages/contracts/package.json
vp install --lockfile-only --ignore-scripts
cargo check --workspace --all-targets --locked
vp run release:smoke
```

Review and commit the preparation only when authorized. Pre-bumping all version
sources makes finalization a no-op when nothing else has changed.

Write the merge and annotated-tag messages to files. Give the merge a summary
of the requested change, implementation, tests, documentation, and review; give
the tag a `BiBCode v<version>` subject and release summary. `git merge -F -`
treats `-` as a filename. Use file-backed messages for both merge and tag so a
failed message read cannot leave a tag on the old `main`.

From the clean checkout of `main`, after the local verification below passes:

```sh
set -eu
merge_message='<merge-message-file>'
tag_message='<tag-message-file>'
tag="v$version"
git merge --ff-only origin/main
git merge --no-ff "$work_branch" -F "$merge_message"
git diff --quiet "$work_branch" HEAD
git push origin main
git tag -a "$tag" -F "$tag_message" main
test "$(git rev-parse "$tag^{commit}")" = "$(git rev-parse main)"
git push origin "refs/tags/$tag"
```

For an ordinary integration, bring `develop` up after pushing `main`. For a
release, do it after publication and finalization, so any workflow version
commit is included. Fetch again and update the clean `main` checkout first:

```sh
git fetch origin
git merge --ff-only origin/main
git merge-base --is-ancestor origin/develop main
```

If the ancestry check succeeds, run `git push origin main:develop`. If it
fails, create a disposable detached worktree at `origin/develop`, merge `main`
there with `--no-ff -F <message-file>`, resolve and verify any conflicts, and
push `HEAD:develop`. Never force `develop` to discard its own content. Remove
only that disposable worktree after a successful integration.

## Stable Release Runbook

1. Confirm the intended version and commit have passed the local verification
   commands below. Create and push the intended tag (or dispatch `stable` with
   that explicit version) with `publish` left at its default `false`.
   If a tag-triggered run exposes a release-controller defect after the native
   artifacts have built, fix and verify the controller on `main`, then start a
   manual repair run for the same version with `publish=false`. The manual
   repair run uses current release tooling only for assembly and verification;
   every desktop and server artifact remains built from the immutable tagged
   commit resolved by preflight.
2. Confirm the six native desktop and six native server build jobs complete and
   that the stable desktop jobs received
   the two signing secrets above. Do not inspect or print their values.
3. Confirm the workflow's descriptor-validation and updater-signature steps
   passed. The workflow must validate exactly six `updater-*.json` descriptors,
   one for each manifest target listed above, before it removes those internal
   descriptors from the public asset set.
4. Let the workflow create the GitHub Release as a **draft**. Before allowing
   publication, inspect its uploaded assets and `latest.json`:

   - `latest.json` has exactly the six manifest target entries above;
   - each target has a nonempty signature and a tag-specific HTTPS payload URL;
   - the release contains a nonempty `.sig` asset for each target;
   - the release contains `latest.json`, two macOS DMGs, two Linux AppImages,
     and two Windows NSIS installers, plus the standalone server archives,
     Linux `.deb`/`.rpm` packages, updater payload archives required by the
     manifest, and the twelve [symbol archives](#symbol-assets); and
   - no private key or passphrase is present in any asset, manifest, or log.

5. Confirm the workflow's comparison of sorted asset names with its generated
   `expected-assets.txt` passed, then perform [Draft inspection](#draft-inspection).
   The workflow must leave the release a draft when the comparison or any
   verification fails. Apply the curated release notes before approval.
6. After a human has inspected the draft, rerun the workflow manually with the
   same version, select the stable channel, and set `publish` to `true`. The
   approval run requires the existing draft, rebuilds the same tagged commit,
   repeats validation, and only then publishes it. It does not upload or
   replace the inspected draft assets. Only numeric non-prerelease stable
   releases are marked latest.
7. Confirm publication through the REST latest endpoint as described below,
   inspect finalization, and synchronize `develop` using the maintainer branch
   flow. Install and smoke-test each ordinary installer on its target operating
   system after publication.

### Failed release runs

For a one-off infrastructure failure, such as an artifact upload timeout after
a successful build, inspect the failed job and rerun only failed jobs:

```sh
gh run view "$failed_run" --log-failed
gh run rerun "$failed_run" --failed
```

Release API permission failures are not transient. The controller uses
`GITHUB_TOKEN` with `contents: write`, but GitHub also requires workflow
authorization when the release target's `.github/workflows/` files differ
from the default branch. The built-in token cannot receive that permission;
both [creation and updates](https://docs.github.com/en/rest/releases/releases)
can fail with 403 or 404. Check the failed job's token permissions and compare
the candidate with freshly fetched `origin/main`:

Publication-capable preflight runs
`bash scripts/check-release-workflow-permission.sh "$RELEASE_REF"` before its
quality gates and native builds. The check resolves the repository's current
default branch through GitHub, freshly fetches it, and compares its workflow
tree with the immutable release commit. Lookup, fetch, or comparison errors
fail closed. A known mismatch explains the required release identity or
unpublished-candidate recovery before native builds consume their budget.
The release job repeats the check immediately before draft/publication steps
because the default branch can advance during builds. Validation-only runs
skip it because they create no release. This is an early diagnostic, not a
permission grant or transaction with GitHub: a later API failure still fails
the job and must be diagnosed. It never changes a tag or release target.

For manual diagnosis against `main`:

```sh
git fetch origin main
git diff --name-only "$tag^{commit}" origin/main -- .github/workflows/
```

Resolve this with an appropriately authorized release identity, or obtain
authorization for the unpublished-candidate replacement below. Pre-creating a
draft does not fix the token's inability to update it. Keep the release target
equal to the verified tag commit, and repeat draft inspection after recovery.

Two timing failures in a row require reproducing and hardening the test before
another release attempt; use [Flaky-test diagnosis](../testing/flaky-tests.md).
The approval run repeats preflight, so retrying until the tag run passes does
not establish reliable publication. Keep product deadlines and assertions
intact.

Replacing a tag is a routine recovery option only for an unpublished candidate
whose failed run produced no draft. It still requires authorization to move
that tag; existing authorization for the same recovery remains valid. Never
retag a published release. If a draft already exists, stop this procedure and
reconcile its commit and assets explicitly; approval requires the inspected
draft to match the tag. A controller-only repair can instead use the manual
repair path in step 1 without moving the tag.

After the fix has passed verification and been merged and pushed to `main`,
confirm all old runs for this tag have stopped. Use an authenticated maintainer
account that can see drafts. A failed API request is not proof of absence:

```sh
set -eu
repository=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
recovery_dir=$(mktemp -d)
gh run view "$failed_run" --json status,conclusion,headSha,event
gh run list --workflow release.yml --branch "$tag" --json status,conclusion,headSha
gh api --paginate "repos/$repository/releases?per_page=100" > "$recovery_dir/releases.json"
jq -s -e --arg tag "$tag" 'all(.[][]; .tag_name != $tag)' "$recovery_dir/releases.json"
old_tag_object=$(git ls-remote origin "refs/tags/$tag" | cut -f1)
test -n "$old_tag_object"
git tag -f -a "$tag" -F "$tag_message" main
test "$(git rev-parse "$tag^{commit}")" = "$(git rev-parse main)"
git push --force-with-lease="refs/tags/$tag:$old_tag_object" origin "refs/tags/$tag:refs/tags/$tag"
test "$(git ls-remote origin "refs/tags/$tag^{}" | cut -f1)" = "$(git rev-parse main)"
```

Inspect the new run's resolved commit. Repeat draft inspection and reapply
curated notes after every new or regenerated draft.

### Draft inspection

Use a fresh inspection directory and the previous stable tag. Compare asset
names with only the version replaced; a changed asset set needs an explanation
from the current workflow, rather than a fixed historical asset count. The first
release with [symbol archives](#symbol-assets) adds twelve
`bibcode-{desktop,server}-symbols-*.zip` names to the previous set:

```sh
set -eu
previous_tag='<previous-stable-tag>'
inspection_dir=$(mktemp -d)
gh release view "$previous_tag" --json assets > "$inspection_dir/previous.json"
gh release view "$tag" --json isDraft,targetCommitish,assets > "$inspection_dir/draft.json"
jq -r --arg old "${previous_tag#v}" --arg new "$version" \
  '.assets[].name | split($old) | join($new)' "$inspection_dir/previous.json" \
  | LC_ALL=C sort > "$inspection_dir/expected-from-previous.txt"
jq -r '.assets[].name' "$inspection_dir/draft.json" \
  | LC_ALL=C sort > "$inspection_dir/uploaded.txt"
diff -u "$inspection_dir/expected-from-previous.txt" "$inspection_dir/uploaded.txt"
jq -e '.isDraft and (.assets | length > 0 and all(.[]; .size > 0))' "$inspection_dir/draft.json"
test "$(jq -r .targetCommitish "$inspection_dir/draft.json")" = "$(git rev-parse "$tag^{commit}")"
gh release download "$tag" --dir "$inspection_dir/assets"
```

For the first release, use the current workflow's asset validation without a
previous-release comparison. Inspect the downloaded stable manifest and verify
its payloads, using the release overlay from the candidate commit:

```sh
repository=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
jq -e --arg version "$version" \
  --arg prefix "https://github.com/$repository/releases/download/$tag/" '
  .version == $version and
  (.platforms | keys == ["darwin-aarch64", "darwin-x86_64", "linux-aarch64",
                        "linux-x86_64", "windows-aarch64", "windows-x86_64"]) and
  all(.platforms[];
      (.signature | type == "string" and length > 0) and
      (.url | startswith($prefix)))
  ' "$inspection_dir/assets/latest.json"
cargo run --locked -p bibcode-updater-verifier -- \
  apps/desktop/src-tauri/tauri.release.conf.json \
  "$inspection_dir/assets/latest.json" "$inspection_dir/assets"
```

Replace the draft's generated notes with the curated `CHANGELOG.md` entry,
without its heading, saved in a notes file. Each new draft needs this step:

```sh
gh release edit "$tag" --notes-file "$notes_file"
```

After human inspection, dispatch approval and wait for the workflow to succeed:

```sh
gh workflow run release.yml --ref main -f channel=stable -f version="$version" -f publish=true
```

Both runs perform preflight and the native build matrices. The current job
budgets are 60 minutes for preflight, 90 for each desktop build, 120 for each
server build, and 25 for the release job, which moves about 470 MB of symbol
archives; these are timeouts, not expected completion durations.
Use Actions job timings to plan the tag and approval runs separately.

After publication, check latest through the REST endpoint. `isLatest` is not a
supported `gh release view --json` field:

```sh
test "$(gh release view "$tag" --json isDraft --jq .isDraft)" = false
test "$(gh api "repos/$repository/releases/latest" --jq .tag_name)" = "$tag"
```

## Local Verification

The commands below remain release-specific checks. Use the
[testing runbooks](../testing/README.md) for repeatable native platform,
packaged UI, external-worktree, visual, process-cleanup, and compatibility
evidence. This release checklist owns publication and release-asset approval;
the runbooks own validation procedure and reporting.

Run the repository gates:

```powershell
vp check
vp run typecheck
vp run -r --concurrency-limit 1 test
vp run release:smoke
```

Before tagging, every workspace package's `test` script must finish
successfully. Check the inventory in `pnpm-workspace.yaml` and each package's
`package.json`, including `scripts`, `oxlint-plugin-bibcode`, and `infra/relay`;
root `vp test` alone omits the package graph and Rust suites. The serialized
preflight stops at the first failed package task. Cargo likewise stops after
the first failing test binary, leaving later binaries unrun; tests within that
binary normally run to completion. A failed early gate is not evidence about
later packages or integration targets. After fixing it, rerun the complete
graph and account for every package.

Linux's case-sensitive filesystem does not reproduce module-resolution
collisions seen on case-insensitive Windows/macOS filesystems. The scripts
package's `module-case-collisions.test.ts` guards tracked module stems,
including `.ts`/`.tsx` pairs that differ only by letter case. Keep it in the
pre-tag test graph and retain native builds; the guard does not cover every
filesystem difference.

Run the updater/release regression set before changing stable release
infrastructure:

```powershell
vp test scripts/tauri-hardening.test.ts scripts/build-desktop-artifact.test.ts scripts/build-tauri-update-manifest.test.ts scripts/ci-platform-contract.test.ts scripts/release-workflow.test.ts scripts/workflow-dependencies.test.ts
vp test apps/web/src/components/settings/SettingsPanels.test.tsx apps/web/src/components/AppSidebarLayout.test.tsx apps/web/src/tauriDesktopBridge.test.ts apps/web/src/components/desktopUpdate.logic.test.ts apps/web/src/state/desktopUpdate.test.ts
node scripts/run-msvc.mjs cargo test -p bibcode-desktop -j 2
```

Build the native artifact for the current operating system:

```powershell
vp run build:desktop
```

On each macOS producer host, verify Finder's rendered application icon from the generated DMG
before publishing it. Build through the artifact wrapper without `--arch` so it
uses the current Mac's architecture. Choose a fresh, empty output directory and
use that same directory for the mount check:

```sh
(
set -e
artifact_dir=release/desktop/macos
node scripts/build-desktop-artifact.ts --platform mac --target dmg --output-dir "$artifact_dir" --verbose

dmg=$(find "$artifact_dir" -maxdepth 1 -type f -name '*.dmg' -print -quit)
test -n "$dmg"
mount_dir=$(mktemp -d /private/tmp/bibcode-icon-dmg.XXXXXX)
attached=0
cleanup() {
  if [ "$attached" -eq 1 ]; then hdiutil detach "$mount_dir"; fi
  rmdir "$mount_dir"
}
trap cleanup EXIT

hdiutil attach -readonly -nobrowse -noverify -mountpoint "$mount_dir" "$dmg"
attached=1
swift scripts/check-macos-app-icon.swift "$mount_dir/BiBCode.app"
)
```

Build a specific release target:

```powershell
node scripts/build-desktop-artifact.ts --platform win --target nsis --arch x64 --output-dir release --verbose
```

Release CI adds `--symbols`, which needs `dump_syms` on `PATH`
(`cargo install dump_syms --locked --version 2.3.9`).

Equivalent root shortcuts are `dist:desktop:dmg`,
`dist:desktop:dmg:arm64`, `dist:desktop:dmg:x64`,
`dist:desktop:linux`, `dist:desktop:win`, and `dist:desktop:win:x64`. The root
package also exposes `dist:desktop:win:arm64`; Windows 11 ARM64 and Windows
10/11 x64 are both supported release targets and require their matching native
release runners.

## References

- [Tauri configuration](https://v2.tauri.app/reference/config/)
- [Tauri updater](https://v2.tauri.app/plugin/updater/)
- [GitHub-hosted runners](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job)
