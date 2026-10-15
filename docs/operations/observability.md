# Observability

BiBCode's production backend is the native Rust server. Axum request handling,
Tokio tasks, provider supervision, terminals, Git, persistence, and desktop
integration emit diagnostics through Rust's `tracing` facade. There is no
production Node.js logger or TypeScript server observability layer.

## Runtime Boundaries

- The Tauri host and in-process server share one native desktop process.
- A headless `bibcode serve` process runs the same Rust server library without Tauri.
- Provider CLIs, terminals, SSH forwards, and `cloudflared` are supervised child
  processes and appear in process diagnostics.
- Browser and WebView clients do not export traces to a remote collector.
- The hosted BiBCode Connect relay has no remote request trace exporter.

Native diagnostics remain local to each BiBCode installation.

## Logs

Rust diagnostics are emitted with `tracing::debug!`, `tracing::warn!`, and
`tracing::error!` at operational boundaries. Native tracing is one
process-wide stream with one subscriber. Each active server runtime owns an
exact sink lease for `userdata/logs/server.log` (or `dev/logs/server.log` for a
source development environment), while the subscriber also retains
human-readable stderr output. Production normally has one active runtime and
one file sink. When several embedded runtimes coexist in one process, every
native tracing event is mirrored to every active runtime-owned sink; those log
files are not runtime-isolated. Public initialization and runtime-owned
initialization that resolve to the same physical log path share one writer,
rotation state, and reference-counted registry entry, so a process event is
written only once to that file. Existing final-component aliases use the
filesystem's fully canonicalized physical target. An absent target is reserved
by its canonical parent identity plus one opaque native filename component.
Every unresolved component retains its exact native spelling on every
platform; case aliases reconcile only after the filesystem opens and identifies
them as one physical target. POSIX filename bytes are never interpreted as
Windows separators, and no platform guesses pre-open case equivalence with a
Unicode fold. Concurrent same-target initialization waits for the reservation
and reuses the resulting writer, including across log rotation. Rotation
advances the writer's identity generation, and the next registration refreshes
the full physical-file index outside the registry mutex before resolving
aliases. Descriptor/path identity checks share one registration-wide budget of
three attempts; repeated target replacement fails startup, rolls the exact
pending reservation back, and wakes same-target waiters. A lease-free entry
stays discoverable while an in-flight event snapshot can still write; the exact
entry is removed only after its last lease and last snapshot drop. A panicking
or failed opener likewise rolls its pending reservation back and wakes
same-target waiters. These guarantees prevent one runtime or in-flight event
from retargeting or tearing down another runtime's logging without accumulating
completed entries. `BIBCODE_LOG` controls the filter and falls back to the
standard `RUST_LOG` behavior, then `info`.

Each interactive `updater.install` request writes one `info` line,
`remote update install requested`, after the host delegate replies, with the
requester's label, OS, address (`unknown` when absent), 8-character session
prefix, and server and target versions. An `error` snapshot adds a `warn` line,
`remote update install request failed`, with the same fields and the error.
Both use the `bibcode_server::remote_update` target and contain no credentials
or full session IDs. Manual install refusals emit neither line.

The desktop notice uses the same requester metadata once per install and
omits unavailable addresses from its copy. Its **Manage devices** action opens
the host's sharing controls. The CI remote-install lane requires exactly one
requester info line for its single install request and retains bounded,
redacted evidence; it never retains bootstrap or pairing credentials.

In headless mode, run the native server from a terminal, or install the
per-user service (`bibcode service install`) whose stdout and stderr go to the
service manager's log (the journal on Linux, `~/Library/Logs/bibcode-server.log`
on macOS):

```bash
bibcode serve
```

From a source checkout:

```bash
cargo run -p bibcode-server -- serve
```

On Linux, starting the desktop in its own user unit makes systemd record how
every run ended, which the Profiler then attaches to an unexplained
`crash.unclean-exit` (a forced kill reads `code=killed, status=9/KILL`):

```bash
systemd-run --user --unit=bibcode-desktop ~/Applications/bibcode.appimage
```

Desktop diagnostics come from the shared in-process Rust runtime and the same
`server.log`; no child server process is involved. The old `server-child.log`
belonged to the removed Node sidecar and is not part of the final architecture.

## Trace Diagnostics

The native server writes actionable RPC failures to
`userdata/logs/server.trace.ndjson`. Records are retained across application
restarts. The writer rotates at 4 MiB and keeps three backups, bounding disk and
memory use while preserving recent failures. The server exposes no
browser-to-server trace ingestion endpoint.

`server.getTraceDiagnostics` reads the current file and its backups on demand.
It reports parse errors, span counts, slow spans, common failures, recent
failures, and warning/error events. A missing file is reported explicitly as
`trace-file-not-found`; it is not presented as a healthy empty trace.

Unary failures and failed stacked Git actions record elapsed server-handler or
action time from the monotonic clock, before diagnostic persistence. A stacked
action records its domain failure once at the action owner; an `action_failed`
stream event is not mistaken for a successful operation merely because it is
carried in a normal RPC chunk. Logging failure never changes the action result.

`durationMs` remains numeric for client compatibility. `durationMeasured` says
whether an occurrence's duration was measured; untimed events and historic
unmarked zero durations are unavailable, not verified instantaneous work.
Unmarked positive legacy durations remain usable. Span summaries include
`measuredCount`; only those samples contribute to totals, averages, maxima and
slow-span rankings. Failure and occurrence counts still include untimed records.
Diagnostics displays unavailable timing as **Not recorded**, and a measured
positive duration below one millisecond as **<1 ms**. For an older server with no
measurement fields, the UI treats zero occurrence durations and summary timing
as unavailable; it can still display positive occurrence durations.

RPC error details are bounded before they reach this store. Authorization
headers, common token/password assignments, and URL credentials are redacted.
Existing unary Git failures retain bounded, redacted stderr so errors such as
malformed `.gitmodules` entries remain actionable after restart. Stacked Git
actions record only closed process facts and fixed failure categories, without
copying command arguments, repository paths, provider output or MR draft text
into the new action diagnostic.

Files operation failures also enter the client's frontend warning/error capture
with their notification title and sanitized detail. Download request and save
failures produce warnings in the desktop host's local log; transfer validation,
archive preparation, and archive stream failures are logged by the server that
owns the workspace. For a remote download these can be different machines.
Signed transfer capabilities are omitted from those diagnostics. An HTTP error
notification retains the bounded server reason and recovery guidance; a failed
archive stream is reported as a failed download, never a completed partial ZIP.

Provider and terminal lifecycle summaries are written as bounded NDJSON under
`userdata/logs/provider/events.log` and
`userdata/logs/terminals/events.log`. Each file rotates at 4 MiB and retains
three files total: the active file and two rotated generations. Records include
lifecycle identifiers, event types, statuses, sequence/PID/exit metadata, and
bounded counts. Provider option-reconciliation records may also include the
provider instance, model, option ID, application method, result, and requested
option value. Terminal output and provider prompts or response payloads are not
written to these operational logs. Do not put credentials or raw environment
values in option values or other diagnostic fields.

## Diagnostic Bundle

The Diagnostics settings page can prepare a timestamped, redacted ZIP through
`POST /api/diagnostics/logs.zip`. Desktop clients open a native save dialog;
browser clients download the response. The archive contains:

- `server.log`, assembled oldest-first from the active native server log and up
  to three retained backups;
- `server.trace.ndjson`, assembled the same way from the active trace file and
  up to three retained backups;
- `frontend.log`, containing the frontend warning/error snapshot supplied by
  the requesting client; and
- `profiler/`, when the profiler directory (`<state dir>/profiler`) holds
  files: the events and findings files and the `crashes/*.json` sidecars.

Provider and terminal operational logs are not included. Each retained server
or trace file is read up to 4 MiB and marked when truncated. The frontend
request and retained frontend content are bounded to 512 KiB; when necessary,
the client omits the oldest frontend records before upload.

Profiler files are bundled newest first, up to 16 MiB uncompressed in total. A
text file is read up to 10 MiB, redacted like the logs, and marked when
truncated; a crash dump is copied byte for byte or left out whole. Anything left
out for size or because it could not be read is listed in
`profiler/OMITTED.txt`, and symbolic links are never followed, including a
linked `profiler` or `crashes` directory. Files are added only while the archive
built so far, with the logs, stays under the desktop save limit of 20 MiB; a
file is counted as if it did not compress.

Crash dumps (`profiler/crashes/*.dmp`) can hold fragments of process memory,
including prompts or tokens, so they are left out unless the request body sets
`"includeCrashDumps": true`. The Diagnostics page sends it only when
**Include crash dumps** is checked in the Profiler section, and the plain
**Download logs** action never does. A body where `includeCrashDumps` is not a
boolean is rejected with `400`. The route needs `orchestration:read`; a request
that includes dumps also needs `orchestration:operate`, because dumps are raw
process memory and are not redacted.

Before writing the ZIP, the server redacts authorization and proxy-authorization
lines, bearer values, common token/API-key/password/secret assignments, and URL
credentials. This is a defensive pattern-based filter, not a proof that every
possible secret format is absent. Review an archive before sharing it outside
the trusted support boundary.

The former Effect logger, `Logger.tracerLogger`, TypeScript `TraceRecord`, and
Node OTLP exporter paths were removed with the TypeScript server. Environment
variables documented for that implementation are not a supported native-server
configuration surface unless they are reintroduced in Rust and covered by
tests.

## Process Diagnostics

Resource diagnostics are host-scoped. Each snapshot describes the selected
environment on one machine; it is not a whole-machine total and never combines
processes from different machines. The reported groups are:

- **Combined**: the complete monitored total for that host, equal to BiBCode Core
  plus External Tooling for CPU, RSS, and process count.
- **BiBCode Core**: the native BiBCode server or combined Tauri host/server root,
  plus UI processes that a desktop adapter can associate reliably with that
  BiBCode instance.
- **External Tooling**: provider CLIs, terminals, helpers, and other processes
  launched or supervised by BiBCode.

For a remote selected environment, Combined, Core, and External describe only
the remote host. A desktop client may also show the always-present local
environment's Core usage separately as **This device**; that local value is not
added to the remote total. Browser clients have no local desktop environment
to report and omit **This device**.

The UI coverage status qualifies whether co-located desktop UI processes are
included in Core:

- `available`: every UI process exposed by the supported platform mechanism was
  sampled;
- `partial`: some UI processes were sampled and a bounded failure explains the
  incomplete coverage;
- `unavailable`: the adapter could not associate any UI process reliably; and
- `notApplicable`: the runtime is headless and has no co-located BiBCode UI.

On Windows, the desktop observer associates WebView2 roots only when they are
current BiBCode descendants carrying WebView2's embedded-browser marker and the
running desktop executable name. Their helper descendants inherit Core UI
ownership.

On macOS, the desktop observer obtains PIDs from every WKWebView owned by the
current Tauri `AppHandle`, then validates each PID with an exact role-specific
executable match and matching resource and jetsam coalition IDs, both nonzero.
Coalition membership is validation rather than a discovery mechanism. A
missing private selector, failed dispatch, missing snapshot row, or unavailable
coalition data
yields `partial` or `unavailable` coverage. This requires neither elevated
privileges nor a new entitlement, but it uses private WebKit and process SPI,
so it is incompatible with a strict Mac App Store public-API-only policy.

On Linux, the desktop observer considers only immediate children of the current
combined Tauri host/server. A strict WebKitGTK Web, Network, or GPU role is
accepted only when `/proc` reports the same parent PID and start identity before
and after resolving an exact role-matched executable. Registered provider and
terminal ownership wins over a conflicting UI observation. Unknown roles,
changed identities, permission failures, and unsupported process topologies
remain unclaimed and produce bounded partial or unavailable coverage. The
observer never claims a generic WebKit process by machine-wide name matching.

Production provider, terminal, provider-helper, managed-endpoint tunnel, and
Android device preview (`android-emulator`, `android-scrcpy`) launchers
register their exact root identity, scope, kind, and bounded label.
Independent Codex/OpenCode helpers and the tunnel use the `helper` kind and
dedicated process groups or Windows Jobs. Descendants inherit the
nearest registered root's attribution. A process with no registered provider or
terminal ancestor that remains descended from the native server is visible as
External Tooling with `unknown` kind and fallback confidence; missing
registration does not make it Core or remove it from the totals.

Attribution and process actions use a stable process identity made from PID and
operating-system start identity captured at launch. Registration fails closed
when that identity cannot be captured; sampling only validates the exact
captured identity, so a reused PID does not inherit the previous process's
ownership. The native sampler remains the source of CPU and memory values;
attribution does not perform a second machine-wide process refresh.

If refresh fails after a successful sample, the client retains the last good
snapshot and its original timestamp, marks it stale, and displays the bounded
failure. Before the first successful sample it shows unavailable placeholders,
not healthy-looking zeroes. Partial and unavailable UI coverage likewise remain
explicit rather than being encoded as zero usage.

The server samples in a background task while clients read diagnostics, and the
diagnostics RPCs only return its latest sample (see
[Performance](../architecture/overview.md#performance)). `readAt` is when the
sample was taken, so the first read after an idle period shows the retained
sample with its original time until the restarted task replaces it. Profiler
`rpc.timing` for these methods therefore no longer includes sampling. A sample
that takes 500 ms or more writes `slow process diagnostics sample` to
`server.log` at `warn`, with the process enumeration, UI observer, and
attribution durations; set `BIBCODE_LOG=info,bibcode_server::diagnostics=debug`
to log them for every sample.

Resource Manager never offers Interrupt or Kill for a Core row. For an eligible
External row, client state is only a request: immediately before signaling, the
server resamples and revalidates the PID/start identity, current ancestry, and
signal eligibility. Stale or reparented identities are rejected.

Use the Diagnostics UI or the corresponding typed RPC methods to:

- inspect current process rows and resource history;
- compare Combined, Core, and External current and historical usage;
- identify registered provider and terminal roots, their inherited descendants,
  and fallback descendants;
- signal a supervised process when the UI permits it;
- verify that shutdown leaves no owned child processes behind.

Packaged-runtime investigations should explicitly confirm that no `node`,
Electron, TypeScript server, or removed native-helper process appears in the
application tree. The desktop runtime measurement reports idle process count,
private memory or its POSIX RSS approximation, working set, and highest-memory
processes. Verify shutdown by checking that its recorded process IDs no longer
exist. Use Resource Manager's attributed CPU display for active-load checks;
the measurement script does not duplicate cross-platform CPU accounting.

## Instrumentation Guidance

### File Manager index phases

File Manager path-index timings use these exact `operation` and `phase` values:

| `operation`                               | `phase`           | When emitted                                                                                                     |
| ----------------------------------------- | ----------------- | ---------------------------------------------------------------------------------------------------------------- |
| `WorkspaceSearchIndex.refresh`            | `cache_hit`       | A ready snapshot is returned without starting Git; elapsed time starts before cache-lock acquisition.            |
| `WorkspaceSearchIndex.refresh`            | `cache_wait`      | A cold caller acquires the per-root build slot; `cache_outcome=shared` means another caller supplied the result. |
| `WorkspaceSearchIndex.refresh`            | `cache_build`     | The physical scan returns success or error, or its result is fenced by a newer generation.                       |
| `WorkspaceSearchIndex.gitSnapshot`        | `git_snapshot`    | The concurrent main and ignored `git ls-files` reads settle.                                                     |
| `WorkspaceSearchIndex.gitSnapshot`        | `ignored_walk`    | Contents beneath Git-reported ignored directory roots are walked.                                                |
| `WorkspaceSearchIndex.gitSnapshot`        | `directory_walk`  | Directories omitted from file listings, including empty directories, are walked.                                 |
| `WorkspaceSearchIndex.filesystemSnapshot` | `filesystem_walk` | The bounded filesystem fallback runs after Git is unsupported or non-authoritative.                              |

Every event has `elapsed_ms`, bounded `entry_count`, and `cache_outcome`.
Outcomes are bounded literals: `hit`, `miss`, `shared`, `build`, `built`,
`stale`, `fallback`, `cancelled`, or `error`. Events never include workspace
paths or entry names. A warm lookup emits only `cache_hit`; shared cold callers
do not repeat the physical scan phases. `cancelled` applies to an observable
search phase such as `git_snapshot`; cache builds have no external cancellation
owner and therefore report only `built`, `stale`, or `error` after returning.
Dropped cache-operation futures do not invent a terminal phase.

Source Control work is separate. Its operations use the `GitVcsDriver.*`
namespace, including `GitVcsDriver.statusReadOwner`,
`GitVcsDriver.statusDetailsLocal.status`, and
`GitVcsDriver.automaticFetch.fetch`; do not attribute those VCS costs to the
File Manager operations above. Record execution-specific timings in validation
reports rather than this living guide.

Add spans and structured fields at meaningful Rust boundaries:

- Axum HTTP and WebSocket RPC requests;
- orchestration command dispatch and committed events;
- provider process start, request, cancellation, and exit;
- SQLite transactions and migrations;
- Git and source-control commands;
- terminal lifecycle and process-tree cleanup;
- relay installation, authentication, and tunnel lifecycle.

Keep high-cardinality values such as thread IDs, paths, and command IDs in span
fields, not metric labels. Never record prompts, credentials, bearer tokens,
pairing links, environment secrets, or raw provider authentication files.

## Verification

For observability changes, run focused Rust tests followed by the repository
gates:

```bash
cargo test -p bibcode-server
cargo clippy --workspace --all-targets -- -D warnings
vp check
vp run typecheck
```

For desktop changes, launch a packaged build, exercise provider and terminal
lifecycle, inspect diagnostics, then close the app and verify that its supervised
process tree is gone.
