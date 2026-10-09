# Profiler — design

Status: approved by the user on 2026-10-09, including the four research-driven
changes marked below. The direction was agreed in a grilling session the same
day (decisions Q1–Q21 below). The research behind the numbers is
[`docs/plans/2026-10-09-profiler-research.md`](../../plans/2026-10-09-profiler-research.md).
Four research findings change earlier answers and are marked **(changes Qn)**.

## Problems

1. The main application crashes on Linux and leaves no logs, so nobody knows
   where it crashed.
2. Provider CLIs, terminals, git and the webview die without a record.
3. Memory leaks and CPU spikes are only visible if someone has the Diagnostics
   page open at the right moment.
4. Worktree removals fail, hang or leave directories and registrations behind,
   and nothing reports it.
5. In Remote Server mode, clients feel unresponsive. Today nobody can tell whether
   the server, the network or the renderer is the slow part. The only client
   signal is the 15 s "slow request" acknowledgement indicator.
6. AI harnesses have no supported way to read any of this live, diagnose it
   and file deduplicated GitHub issues.

## Scope agreed in the grilling session

| #   | Decision                                                                                                                                                                                                |
| --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Q1  | One branch, one PR, built from this approved spec.                                                                                                                                                      |
| Q2  | Catch every crash kind: Rust panic, native fault, OOM/SIGKILL, webview and subprocess deaths.                                                                                                           |
| Q3  | One **Profiler** toggle per environment, stored in that server's settings. Clients connected to it send their metrics to it.                                                                            |
| Q4  | Storage is my choice: NDJSON (Decision 4).                                                                                                                                                              |
| Q5  | Access is my choice: CLI plus MCP (Decision 9).                                                                                                                                                         |
| Q6  | Leak detection by trend heuristics on memory time series.                                                                                                                                               |
| Q7  | No fixed latency defaults; thresholds learned per method (Decision 7).                                                                                                                                  |
| Q8  | Worktree problems: failed or timed-out removals, leftover directories, catalog entries whose directory is gone, git-registration mismatches.                                                            |
| Q9  | The skill searches for duplicates first. It comments on a duplicate it can extend; otherwise it drafts an issue and asks before creating it.                                                            |
| Q10 | Privacy first.                                                                                                                                                                                          |
| Q11 | Out-of-process crash monitor: the same binary relaunched as `--crash-monitor` (explicit exception to the AGENTS.md "no native helper sidecar" rule).                                                    |
| Q12 | Per-method learned baseline plus an absolute floor.                                                                                                                                                     |
| Q13 | Overhead ≤ 1% CPU on average; 5 s sampling; 10 min worktree scan; 7 days or 256 MB per environment.                                                                                                    |
| Q14 | The toggle persists across restarts; a status-bar indicator shows while recording.                                                                                                                      |
| Q15 | Diagnostics page: toggle, status line, Open folder, Export bundle.                                                                                                                                      |
| Q16 | Client metrics: my choice (Decision 8).                                                                                                                                                                 |
| Q17 | Skill source of truth under `docs/skills/`, harness folders reference it.                                                                                                                               |
| Q18 | Shipped binaries stay stripped; CI attaches Breakpad `.sym` files to each GitHub release.                                                                                                               |
| Q19 | The monitor follows the toggle live, without restarting the app.                                                                                                                                        |
| Q20 | Move **all** repository skills to `docs/skills/`.                                                                                                                                                       |
| Q21 | The Diagnostics page also lists the 20 newest findings.                                                                                                                                                 |

## Decisions

### 1. Toggle and lifecycle

Add `enableProfiler: false` to `ServerSettings` (`packages/contracts/src/settings.ts`)
and to the Rust settings state and patch (`apps/server/src/server_settings/mod.rs`),
following `enableTerminalAgentActivity`. The existing settings update RPC carries it.
No new settings RPC is added.

A `ProfilerRuntime` in `apps/server/src/diagnostics/profiler/` owns everything
below. It starts or stops its sampler, detectors and crash-monitor child when the
setting changes, so no restart is needed. When the toggle is off, nothing runs
except the always-attached crash handler (Decision 2) and the clean-shutdown
sentinel (Decision 3).

On desktop, the **local** environment's setting governs the desktop host's crash
monitor, because the local server runs inside the host process. Desktop `main()`
reads that setting from the local data root's settings file before Tauri starts,
so a crash during startup is still captured once the toggle is on. WSL, SSH and
remote environments run their own `bibcode` process, which follows its own setting.

The status bar shows a "Profiler recording" segment for every connected
environment whose profiler is on.

Rejected: a toggle that switches itself off after N hours. A crash can only be
captured if the profiler is already on when it happens (Q14).

### 2. Native crash capture **(changes Q11 and Q19)**

Use `crash-handler` (≥ 0.8.1, < 0.9) and `minidumper` 0.11.

- **Attach once, never detach.** The in-process handler is attached early in
  `main()` in both binaries, whatever the toggle says. Detaching on Linux
  restores the handlers that existed at attach time, which removes the handlers
  JavaScriptCore installed afterwards, and detaching can currently panic
  (upstream PR #118). The toggle only starts and stops the **monitor child**.
  The crash callback checks a `static AtomicBool` and an atomically swappable
  `Client`. With no client, it returns `Handled(false)`, so the OS path
  (systemd-coredump, WER) still runs.
- **Cost when off:** linking the crate interposes `pthread_create` even when
  nothing is attached (upstream issue #123). This is the only cost of "off".
- **Dispatch.** `--crash-monitor <socket> <dump-dir>` is handled next to the
  existing open-url shim. In desktop `main.rs` that is before Tauri and the
  single-instance plugin. In server `main.rs`, `#[tokio::main]` becomes a plain
  `fn main` that dispatches before building the runtime. The monitor puts itself
  in its own process group, ignores SIGINT/SIGTERM, exits when its parent dies,
  and returns `LoopAction::Continue` after each dump. The client reconnects after
  each dump, because a `Client` is single-use on Linux (upstream issue #99).
- **One handler per process.** The desktop host attaches it for itself and its
  in-process server. The standalone `bibcode` binary attaches its own. Desktop
  calls `bibcode_server::diagnostics::profiler::crash::attach()`, which follows
  the permitted desktop → server dependency direction.
- **Compromised-context rule.** The callback never logs, allocates or takes locks.
- **Linux ptrace.** At startup the monitor probes whether it can attach (Yama
  scope, containers). If it can't, the capability header records "native dumps
  unavailable", and Decisions 3 and 5 still apply.
- **Windows arm64.** `minidump-writer` marks it unimplemented, so dumps stay
  disabled there until a native runtime test passes. Panic records still work.
- **Storage.** Each dump goes to `profiler/crashes/<id>.dmp` with a `<id>.json`
  sidecar holding version, OS, process kind and the last 50 profiler events
  before the crash. At most 20 dumps are kept, oldest deleted first, within the
  256 MB cap.

Rejected: using only the OS records (they are unreachable without admin on
Windows); an in-process signal handler that writes its own stack (unsafe, not
reliable); `sentry-minidump` and `minidumper-child` (they pin crash-handler 0.7,
which has the macOS 100%-CPU child bug).

### 3. Panics, unclean exits and OS crash records **(changes Q11)**

**Panic hook.** It chains to the previous hook and records up to 64 raw
instruction pointers as `(module, debug_id, offset)`, plus thread name and a
redacted message. It does **not** abort: BiBCode relies on `panic = "unwind"`
and recovers panics at many `catch_unwind` and `JoinError::is_panic` sites. The
module mapping uses `dladdr` plus a startup-cached ELF build-id or Mach-O UUID
on Linux and macOS, and the module base address on Windows. That is the same
addressing as Breakpad, so the same `.sym` files symbolicate panics and
minidumps. Panics are recorded even when the toggle is off, because a panic is
rare and cheap to record.

**Clean-shutdown sentinel.** At startup the process atomically writes
`profiler/session.json`:

```
{ state, pid, bootId, exe, version, startedAt, oomKillBaseline }
```

An orderly shutdown sets `state: "clean"`. On the next start, a leftover
`running` state is classified as follows:

| Evidence on next start                                 | Finding                                                     |
| ------------------------------------------------------ | ----------------------------------------------------------- |
| Same boot, a minidump or OS crash record for that pid  | `crash.native` with location                                |
| Same boot, `oom_kill` count increased                  | `crash.oom` (Linux)                                         |
| Same boot, no record                                   | `crash.unclean-exit` (SIGKILL, power loss or a missed dump) |
| Different boot ID                                      | `crash.system` (OS crash or power loss)                     |

The sentinel runs even when the toggle is off, so the next start can always
report "the previous run did not exit cleanly".

**Collecting OS crash records** happens on the next start, read-only and
bounded to 5 s:

- **Linux:** `coredumpctl --json=short list` to find the entry, then
  `journalctl -a -o json MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1 COREDUMP_PID=<pid>`
  for the backtrace (`-a` is required). Match on `COREDUMP_EXE`. Read cgroup v2
  `memory.events` `oom_kill` from `user@UID.service`.
- **Windows:** Application log Events 1000 and 1001 for our executable.
- **macOS:** `~/Library/Logs/DiagnosticReports/*.ips` with `bug_type == "309"`,
  matched by process name and time.

If a facility is missing (no systemd-coredump, WSL, Alpine), the
`crash.unclean-exit` finding says so.

Rejected: Zed's aborting panic hook, which would turn recoverable panics into
outages; WER `LocalDumps`, which can only be set under HKLM and so needs admin.

### 4. Storage

Add a new `ProfilerStore` in `apps/server/src/diagnostics/profiler/store.rs`.
`TraceDiagnosticsStore` is not reused, because it writes synchronously on the
RPC path.

- **Hot path:** `try_send` into a bounded `mpsc` channel of 2048 events. When
  the channel is full, the event is dropped and an `AtomicU64` counter is
  incremented. Drops are written into the file as `telemetry.dropped` events.
- **Writer:** one dedicated OS thread, a 64 KiB `BufWriter`, flushed every 2 s.
  Files rotate at 10 MiB, and `sync_data` runs only on rotation and shutdown.
  Rotation is written by hand, because `tracing-appender` has no size-based
  rotation and `file-rotate` silently discards data on errors.
- **Retention:** 7 days or 256 MB per data root, counting dumps; the oldest file
  is deleted first.
- **Layout under `<data root>/profiler/`:**
  - `events-<seq>.ndjson`: samples, RPC records, client batches, worktree audit
    events.
  - `findings.ndjson`: one line per finding state change.
  - `session.json`: the sentinel.
  - `capabilities.json`: what this host can capture.
  - `crashes/`: dumps and sidecars.
- **Event envelope:**
  `{ v: 1, ts, seq, kind, source: "server"|"client:<clientId>", envId, ...payload }`.
- **Torn lines:** readers skip a torn final line.

Rejected: SQLite. Telemetry is append-heavy and read in bounded windows, and
NDJSON can be read with `tail` and `jq` while the app is down.

### 5. Process sampling and leak detection

A new 5 s loop in `ProfilerRuntime`:

- **Targeted refresh.** It refreshes only BiBCode's own process tree with
  `ProcessesToUpdate::Some(pids)` and `.without_tasks()`, on a blocking thread
  with one long-lived `System`. The PIDs come from `ProcessAttributionRegistry`
  plus a descendant rediscovery every 60 s. The existing on-demand
  `DiagnosticsMonitor` is untouched.
- **Process kinds.** Every process is labelled server, desktop-host, webview,
  provider:<driver>, terminal, git or other. Webview children (WebKitWebProcess,
  WebContent, msedgewebview2) are labelled during rediscovery.
- **Memory metric.** Leak detection uses private memory, not RSS: Linux
  `RssAnon + VmSwap` from `/proc/<pid>/status`, Windows `PrivateUsage`, macOS
  `phys_footprint` via `proc_pid_rusage`. RSS and CPU% (normalized by core count)
  are recorded alongside.
- **Self-throttling.** Each sample records the profiler's own CPU. A tick that
  takes over 50 ms doubles the interval until ticks are fast again.
- **Leak rule (RESIN-style).**
  - Samples are reduced to 1-minute medians, after a 15-minute warm-up.
  - Detection runs on a rolling 10-minute minimum, so garbage-collection and
    allocator sawtooth don't count as growth.
  - **suspect:** Theil–Sen slope ≥ max(10 MB/h, 2% of baseline per hour) over
    1 h, and Mann–Kendall Z > 2.33.
  - **likely:** 6 h of growth with R² > 0.8, or growth of ≥ 50% and ≥ 200 MB
    since baseline.
  - A step change larger than 3σ resets the baseline.
  - It takes two consecutive hits to raise a finding, and each process raises
    at most one per 6 h.
- **CPU findings.** `cpu.sustained` when a process stays above 80% of one core
  for 5 minutes.

### 6. Exit-status watch for subprocesses

The places that spawn provider CLIs, PTYs and git already observe exit status
through the process supervision code. When the profiler is on, they also report
unrequested deaths:

- A signal exit, or a non-zero exit outside a requested stop, becomes a
  `subprocess.crash` finding with driver, signal or exit code, uptime and the
  last memory sample.
- `SIGKILL` that BiBCode didn't send is labelled "likely OOM / oomd".
- On Linux, the matching coredump record is harvested in the background.
- On desktop Linux, WebKitGTK `web-process-terminated` (reached through
  `WebViewExtUnix::webview()`) becomes a `crash.webview` finding with its reason
  and the coredump record. This is likely what users see as "the app crashed".
- macOS uses wry's web-content-terminate handler. Windows uses WebView2
  `ProcessFailed`, whose report folder holds a Chromium minidump.

### 7. Slow-request detection

**Server side.**

- **Every request is timed.** The existing unary `register` wrapper in
  `rpc/session.rs` already times every handler but only records failures. When
  the profiler is on, it also feeds a per-`(method, outcome)` latency window.
- **Window.** A ring of six 5-minute buckets, each holding about 100 log-scale
  buckets, with no new dependency.
- **Stream methods** record time to first item.
- **Queue time.** The time from frame receipt to handler start is recorded as
  `queuedMs`.

**Rule.**

- **Relative tier:** after at least 100 samples, flag a request when it takes
  longer than `max(3 × p95, floor)`. The floor is 250 ms for interactive methods
  and 1 s for background methods. The p95 excludes the current bucket.
- **Absolute tier:** always warn at 1 s and flag 10 s as severe, so a method that
  is always slow can't make slowness its own baseline.
- **In-flight check:** requests still running are checked every tick, so a hang
  is flagged before it finishes.
- **Saturation:** when five or more methods trip in the same tick, one
  `rpc.saturation` finding is raised instead.
- **Launch mode:** this PR ships the relative tier in **log-only** mode (events,
  no findings) and promotes it to findings once each method has its baseline
  across real sessions, as the research recommends. The absolute tiers raise
  findings from day one.
- **Join log:** a bounded ring of the last 4096 `(correlationId, method, serverMs,
  queuedMs)` records is kept so client reports can be matched (below).

**Client side, which explains the Remote Server waits.** The client already
observes every RPC through `EnvironmentRpcRequestObserver` in
`packages/client-runtime/src/rpc/client.ts`.

- **What it measures:** round-trip time and render time. Render time is from
  response receipt to the second `requestAnimationFrame` after the state update.
- **What it sends:** every 10 s, one `server.reportProfilerClientBatch` call with
  compact per-method histograms, and individual records only for requests over
  250 ms or over the absolute tiers.
- **How the server matches them:** it pairs each record with its own join log by
  `correlationId`, then computes `transport ≈ rtt − serverMs − queuedMs`. The
  finding names the slow part: `server`, `network` or `render`.
- **No clock sync:** only durations cross the wire, never timestamps.
- **Hidden documents and system sleep:** samples that span either are discarded.

The wire request ID is not visible to the observer, and a trace ID covers
several RPCs (for example repeated `uploads.append` calls), so neither is a
reliable key. While the profiler is on for an environment, the client therefore
sends a per-request correlation ID in the `x-bibcode-profiler-rid` RPC header
(`RpcClient.withHeaders`). A record without a join entry is marked
`joinedBy: "none"` and its slow part `unknown`; there is no time-based guess.

Rejected:

- Adding `serverMs` to every RPC response envelope. It is a wire protocol change
  with parity-fixture churn, and the server-side join gives the same answer.
- Mean + kσ. Latency distributions are skewed, so outliers would inflate the
  very threshold meant to catch them.
- `hdrhistogram`. It would be a new dependency, and log-scale buckets are enough
  at a 3× multiplier.

### 8. Client responsiveness and memory

The client checks `PerformanceObserver.supportedEntryTypes` and reports the set
of capabilities it found in every batch.

- **Long Animation Frames / long tasks** are used where available (WebView2,
  Chromium).
- **Elsewhere** (WebKitGTK, WKWebView, Firefox): a 500 ms `setTimeout` drift
  heartbeat. Arriving more than 50 ms late counts as a stall. The heartbeat is
  paused while the document is hidden.
- **Event Timing (INP)** is used where present: WebKitGTK ≥ 2.52, Safari ≥ 26.2,
  Chromium, Firefox. The threshold is 104 ms.
- **JS heap:** `performance.memory` is reported where it exists (Chromium only).
- **Webview process memory** is always measured by the Rust side of the host the
  client runs on (Decision 5), never estimated in JS.
- **Findings:** `client.stall` when stalls sum to more than 1 s in a 10 s window,
  and `client.interaction` when INP is over 500 ms.
- **Which environment receives which data:** request records go to the
  environment that served the request. Client-wide metrics go to every connected
  environment whose profiler is on.
- **Production builds only:** React's Profiler build is never shipped.

### 9. Worktree audit

This is designed from the source, not the research. The availability registry
and `WorktreeRemovalErrorReason` are the sources of truth.

- **Removal outcome.** The existing `worktree.remove` and
  `worktree.removeFromBibCode` handlers emit a `worktree.removal` event for every
  outcome. It includes the reason, duration, mode, and whether the
  admission-drain timeout, `cleanup-capacity` or `operation-capacity` was hit.
  Failures and timeouts become `worktree.removal-failed` findings. The paths
  in the event are redacted (Decision 11).
- **Scan every 10 minutes (read-only; it never prunes or deletes):**
  - `missing-registered` and `missing-unregistered` entries → `worktree.missing`.
  - A `removing` guard older than 10 minutes → `worktree.stuck-removal`.
  - A directory under the worktree base directory with no catalog entry and no
    git registration → `worktree.orphan-dir`.
  - `git worktree list --porcelain` compared with the catalog for each project
    → `worktree.registration-mismatch`.
  - A process that BiBCode owns and whose cwd is inside a worktree being removed
    or already removed → `worktree.held-open`. This uses the sampled process tree
    and is Linux and macOS only.

Each scan runs on the existing operation runtime, with at most one scan at a time.

### 10. Findings model

A finding is the unit that the UI, the CLI, the MCP server and the skill all
work on.

```
{ id, fingerprint, kind, severity: info|warn|error, firstSeen, lastSeen, count,
  envId, source, title, summary, evidence: { eventRefs["<sid>:<seq>"], crashId?, ... }, state: open|resolved }
```

The `fingerprint` is the first 12 hex digits of `sha256` over stable fields, and
never includes paths or timestamps:

- **Crash:** kind, module, debug_id, and the top three frame offsets (or the
  symbolicated function names when available).
- **Slow request:** kind, method, outcome and the slow part.
- **Leak:** kind, process kind and executable name.
- **Worktree:** kind and reason.

A repeat of the same fingerprint increments `count` and updates `lastSeen`, and
does not create a new finding.

### 11. Privacy

Redaction happens in the collector, before anything is written, so the UI, CLI,
MCP and skill all share one policy.

- **Never recorded:** prompt, message or terminal content.
- **Command lines:** reduced to executable name plus known-safe flags.
- **Paths and secrets:** home directories become `~`, and the existing
  `redact_sensitive_text` is applied. ANSI and control characters are stripped,
  and fields are truncated.
- **Minidumps** contain stack memory, so they never leave the machine unless the
  user explicitly asks. "Export bundle" excludes them by default and has a
  separate "include crash dumps" checkbox with a warning.
- **The skill** shows every issue draft and comment before posting it (Decision 13).

### 12. Access for AI harnesses

`apps/server/src/diagnostics/profiler/reader.rs` is one read-only query layer
over the profiler directory. It works while the server is down, which is
exactly when crash data matters.

**CLI:**

```
bibcode profiler status|findings|events|crashes|symbolicate --json
         [--since 1h] [--kind ...] [--limit N] [--follow --duration 60s]
```

- `--follow` is always bounded.
- The desktop executable dispatches `profiler …` before Tauri, the same way it
  handles open-url, because desktop installs ship no separate `bibcode` on PATH.

**MCP:** `bibcode profiler mcp` runs a **stdio** MCP server.

- **Tools:** `profiler_status`, `profiler_findings`, `profiler_events`,
  `profiler_crash`, `profiler_symbolicate`. All are read-only, paginated under
  25k tokens, and described as untrusted application data.
- **Authentication:** filesystem permissions. Nothing new is exposed over the
  network.
- **Remote hosts:** `ssh host bibcode profiler mcp`.
- **Implementation:** reuses the JSON-RPC handling style of the existing
  hand-rolled dispatcher in `production/connect_mcp.rs`. No new dependency.

**Symbolication:** `profiler symbolicate <crashId>` downloads the release's
`symbols-<version>.zip` asset (Decision 14) into a cache, then runs the
`minidump-processor` library. `minidump-processor` is added as a dependency of
the CLI path only; the server does not use it at runtime.

Rejected:

- rmcp 3.5.1 over HTTP: a second MCP stack, a new token type, a new network
  surface, and it only works while the server is up.
- Adding tools to the existing `/mcp` endpoint: its credentials are scoped to
  one provider session, which is the wrong audience.

### 13. The skill

- **Location:** `docs/skills/bibcode-profiler/` holds the single source:
  `SKILL.md` plus references for event and finding schemas, triage recipes per
  finding kind, and the issue template.
- **Harness stubs:** `.agents/skills/bibcode-profiler/SKILL.md` and its
  byte-identical copy `.claude/skills/bibcode-profiler/SKILL.md` contain
  frontmatter and "Read and follow `docs/skills/bibcode-profiler/SKILL.md`".
  `bibcode-release` and `issue-delivery` move the same way (Q20), and AGENTS.md
  links are updated. `scripts/repo-skills-sync.test.ts` keeps passing because
  the stubs match.
- **Authoring:** created with skill-creator, including trigger evals.

The skill has three modes:

1. **Monitor.** It enables the profiler only after confirming with the user.
   Then it polls `profiler findings --follow --duration` in bounded loops and
   summarizes new findings live.
2. **Diagnose.** It runs one sub-agent per open finding, or works sequentially
   when the harness has no sub-agents. Each agent:
   - collects the evidence events;
   - symbolicates crashes;
   - maps frames, methods or processes to source files;
   - forms a root-cause hypothesis with confidence and a reproduction.
3. **Report.** For each diagnosed finding, it:
   - searches GitHub (`origin` repo, overridable) for a duplicate, first by the
     `profiler:<fingerprint>` label, then by the
     `<!-- profiler-fingerprint: … -->` marker, then by title similarity;
   - for a duplicate it can extend, comments with the new evidence directly
     (Q9);
   - otherwise drafts an issue (title, environment, evidence, symbolicated
     stack, suspected source, reproduction, fingerprint marker), shows it, and
     runs `gh issue create` only after the user confirms.

Every draft passes the redaction check from Decision 11.

### 14. CI symbol pipeline **(changes Q18 mechanics)**

`strip = "symbols"` stays in the shipped profile, but the release build changes
so `dump_syms` sees the symbols before they are stripped:

- **Build with symbols.** CI builds with `CARGO_PROFILE_RELEASE_STRIP=none` and
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`.
- **Desktop.** `scripts/build-desktop-artifact.ts` splits `tauri build` into
  `tauri build --no-bundle`, then `dump_syms --inlines --store symbols --check-cfi`,
  then a platform strip, then `tauri bundle`.
  - **Linux:** strip, with `-Wl,--build-id` added.
  - **macOS:** `split-debuginfo = "packed"` so a dSYM is produced; `dump_syms`
    reads the dSYM.
  - **Windows:** `dump_syms` reads the PDB; there is nothing to strip.
- **Server.** The standalone `bibcode` build, including the `ubuntu:20.04`
  container job, runs `dump_syms` on the host after the `chown`, then strips.
- **Release assets.** All `.sym` stores are zipped into one
  `symbols-<version>-<platform>-<arch>.zip` per platform and attached by the
  existing release jobs.
- **Smoke test.** A new CI test, on every OS, crashes a small test binary under
  the monitor and symbolicates the dump with the generated symbols.

Unverified: whether `tauri bundle` avoids a cargo rebuild on every platform.
This is the step most likely to break a release, so it gets its own release-job
dry run before merge.

### 15. Diagnostics page and status bar

`DiagnosticsSettings.tsx` gets a **Profiler** section:

- the toggle (off by default) with a one-line explanation of what it records and
  that it stays local;
- a status line: recording since, disk use, event count, dropped events, last
  crash;
- **Open folder** (desktop and local environment only; disabled with a reason
  otherwise);
- **Export bundle**, which extends the existing `/api/diagnostics/logs.zip` with
  the profiler directory, dumps excluded by default;
- the 20 newest findings, each with kind, severity, count and last seen.

The data comes from one new RPC, `server.getProfilerStatus`. The status bar gets
a "Profiler recording" segment. Existing `ResourceDiagnosticsSections` charts
are reused unchanged.

## Wire and contract changes

- **Settings:** `ServerSettings.enableProfiler`.
- **RPC:** `server.getProfilerStatus` (unary) and
  `server.reportProfilerClientBatch` (unary, small bounded payload: at most 64
  records and 64 histograms per batch).
- **Pinned counts:** RPC method, fixture and fingerprint counts are bumped in
  `apps/server/tests/rpc_wire.rs`, `export-rust-rpc-fixtures.test.ts` and
  `rpcRustParity.test.ts`.
- **Response envelope:** unchanged (Decision 7).

## Architecture and policy documents to update

- **`AGENTS.md`:**
  - "no native helper sidecar" gets one exception: the same BiBCode binary
    relaunched as `--crash-monitor` while the profiler is on;
  - skill links move to `docs/skills/`.
- **`docs/architecture/overview.md`:** profiler runtime topology, crash monitor
  process, data layout and privacy boundary.
- **`docs/architecture/rpc-and-orchestration.md`:** the two RPCs and the
  timing-join design.
- **`docs/architecture/worktree-catalog.md`:** the audit events and scan.
- **`docs/architecture/remote.md`:** where client metrics go in remote mode.
- **`docs/testing/cross-platform-validation.md` and the three `*-desktop.md`
  runbooks:** a crash-smoke procedure for each OS (crash, verify finding,
  symbolicate) and the profiler toggle in native visual validation.
- **`docs/reference/scripts.md`:** the split desktop build and the symbols step.
- **`docs/operations/release.md`:** the symbols asset.

## Testing

- **Rust unit tests:**
  - leak detector: steady, stair, sawtooth and step-change series, with the
    expected suspect / likely / none results;
  - latency window and both tiers;
  - fingerprint stability;
  - store rotation, drop counting and torn-line reading;
  - sentinel classification;
  - parsers for coredumpctl and journal JSON, `.ips` and Windows event XML, using
    fixtures recorded from real systems;
  - redaction.
- **Rust integration tests:**
  - toggling the setting starts and stops the runtime with no restart;
  - `getProfilerStatus`, and a `reportProfilerClientBatch` join;
  - worktree removal emits outcome events;
  - the audit scan flags a planted orphan directory and a stuck `removing` guard;
  - the CLI reader works with the server stopped.
- **Crash smoke (per OS, in CI):** a test binary under the monitor writes a dump,
  and symbolication names the planted function.
- **Web tests:**
  - the Diagnostics Profiler section: toggle, status, findings, the disabled
    Open folder with its reason;
  - the status-bar segment;
  - the client collector: capability detection, heartbeat stall math and batch
    bounds.
- **Overhead check:** a test asserts that a sampler tick on a 20-PID tree stays
  under 5 ms on Linux CI. Windows and macOS timing is recorded in the execution
  report.
- **Gates:** `vp check`, `vp run typecheck`, `cargo fmt --all --check`, Clippy
  with `-D warnings`, the React review with `vercel-react-best-practices`, and a
  `UI.md` review.

## Build order within the PR

1. Store, event envelope, findings model, redaction, reader and CLI.
2. Settings toggle, `ProfilerRuntime`, sentinel, panic hook, status RPC, and the
   Diagnostics section with the status bar.
3. Sampler and leak and CPU detectors; subprocess and webview exit watch.
4. Server latency window, client collector and batch RPC, and the join.
5. Worktree audit.
6. Crash handler and monitor, OS crash-record collection, and the CI symbol
   pipeline with the crash smoke test.
7. Stdio MCP and symbolication.
8. Skill relocation and the new skill (skill-creator, evals).
9. Documentation, runbooks, gates and Codex review.

## Residual risks

- Windows, macOS and aarch64 crash capture can only be proven in CI and on
  native machines; only Linux x86_64 was run end to end during research.
  Windows arm64 dumps stay disabled.
- Whether a JavaScriptCore stack overflow reaches the handler is untested.
  Stack overflows may be reported only as `crash.unclean-exit` plus the
  coredump record.
- The `.sym` asset size for the real binaries is unknown; measure it on the
  first CI run.
- The relative latency tier stays log-only until real baselines exist.
- Whether WebView2 replaces the host's exception filter is unverified; the
  filter is logged after the webview is created.
