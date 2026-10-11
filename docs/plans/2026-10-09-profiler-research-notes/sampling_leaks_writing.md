# Low-overhead resource sampling, leak detection from time series, and non-blocking telemetry writing (Rust/Tokio, Linux/Windows/macOS)

Context: the repo already pins `sysinfo = "0.39.6"` (workspace `Cargo.toml:59`, used by `apps/server`). All sysinfo claims below were checked against the 0.39.6 source in `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/sysinfo-0.39.6/src`. A local microbenchmark was run on Linux 7.2.5, AMD Ryzen AI MAX+ 395, 32 logical CPUs, 3,904 process+thread entries (scratchpad, release build, sysinfo 0.39.6). Relative source paths below resolve against https://github.com/GuillaumeGomez/sysinfo/tree/v0.39.6/src and https://github.com/tokio-rs/tokio/tree/tokio-1.53.1/tokio/src.

## Q1. sysinfo refresh cost, CPU% semantics, native alternatives, and which memory metric to use

### Takeaway

Never call `refresh_processes(All)` every tick. Use `ProcessesToUpdate::Some(&tree_pids)` with `ProcessRefreshKind::nothing().with_cpu().with_memory()`, which costs about 15 µs per PID on Linux. Rediscover the child tree rarely (every 30–60 s) with an `All` scan that uses `.without_tasks()`, which costs about 7 ms on a box with 3.9k entries. That keeps a 5 s sampler well under 0.1% of one core. For leak detection, use a "private footprint" metric rather than RSS: Linux RssAnon (+VmSwap), Windows PrivateUsage (commit charge), macOS `phys_footprint`. Chromium made the same choice. sysinfo's `memory()` reports RSS / WorkingSet / `pti_resident_size`, so the private metric needs a small per-OS native read.

### Cited Findings

**sysinfo API and internals (0.39.6)**

- `refresh_processes(All, true)` is equivalent to `refresh_processes_specifics` with memory, CPU, disk usage and exe. The docs say refreshing all processes and their tasks "can be quite expensive", and suggest `ProcessRefreshKind::everything().without_tasks()` on Linux. sysinfo keeps `stat` files open by default on Linux (`set_open_files_limit` changes this). — [docs.rs sysinfo System](https://docs.rs/sysinfo/latest/sysinfo/struct.System.html)
- `remove_dead_processes` only considers processes included in the update. If you refresh PIDs 1, 2, 3 and PIDs 2 and 7 are dead, only 2 is removed. — [docs.rs sysinfo System](https://docs.rs/sysinfo/latest/sysinfo/struct.System.html)
- `MINIMUM_CPU_UPDATE_INTERVAL` is 200 ms on Linux, macOS and Windows, and 100 ms on BSD. The docs say: "If refreshed too often, the CPU usage of processes will be `0` whereas on Linux it'll always be the maximum value (`number of CPUs * 100`)." — [docs.rs constant](https://docs.rs/sysinfo/latest/sysinfo/constant.MINIMUM_CPU_UPDATE_INTERVAL.html); values from source `unix/linux/system.rs:114`, `unix/apple/system.rs:74`, `windows/system.rs:41`, `unix/bsd/system_common.rs:51`.
- `Process::cpu_usage()` can exceed 100%. On a multi-core machine it is a percentage of one core, and the docs say to divide by the CPU count for 0–100%. It needs two refreshes because it is computed from a time difference. In the Linux source, `compute_cpu_usage` = Δ(utime+stime) / (Δ global CPU jiffies / nCPUs) × 100, clamped to nCPUs×100. The first refresh returns 0 (`old_utime == 0 && old_stime == 0`). — source `common/system.rs` (doc on `cpu_usage`), `unix/linux/process.rs:346-361`, `unix/linux/system.rs:188-206`
- Linux `ProcessesToUpdate::Some(pids)` touches only `/proc/<pid>` for the listed PIDs and does not `read_dir("/proc")`. CPU% still refreshes global `/proc/stat` times (`update_procs_cpu`). — source `unix/linux/process.rs:842-879`, `unix/linux/system.rs:188-206, 278-293`
- Windows `ProcessesToUpdate::Some` still takes a full `CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS)` of every process and filters it with a linear `filter.contains(&pid)`. Targeted refresh therefore saves the per-process `OpenProcess`/`GetProcessMemoryInfo` work but not the system-wide snapshot. — source `windows/system.rs:198-290`
- Per-platform meaning of `Process::memory()` in sysinfo: Linux uses statm `resident` (RSS) (`unix/linux/process.rs:749-780`). Windows uses `PROCESS_MEMORY_COUNTERS_EX.WorkingSetSize`, and `virtual_memory()` = `PrivateUsage` (`windows/process.rs:313-325`). macOS uses `proc_taskinfo.pti_resident_size` from `proc_pidinfo` (`unix/apple/macos/process.rs:447,796`).

**Local measurements (Linux, sysinfo 0.39.6, 3,904 proc+task entries, 32 CPUs), mean per call**

- `refresh_processes(All, true)` (default kind): **42.3 ms**
- `All`, cpu+mem (tasks included): **33.0 ms**
- `All`, cpu+mem `.without_tasks()`: **7.3 ms**
- `All`, `ProcessRefreshKind::nothing()` (still walks tasks): **22.3 ms**
- `Some(1 pid)`, cpu+mem: **13.9–14.9 µs**. `Some(1 pid)`, mem only: **9.2 µs**. `Some(11 pids)`, cpu+mem: **130–199 µs** across two runs. Some of these PIDs were heavily multithreaded, and their tasks were refreshed too.
- With `.without_tasks()`: `Some(1 pid)`, cpu+mem: **12.7 µs**. `Some(11 pids)`, cpu+mem: **74 µs** (about 7 µs per PID). The thread-group leader's `/proc/<pid>/stat` already aggregates every thread's utime/stime, and statm is per-process, so refreshing tasks adds cost without adding signal here.
- Raw reads of the current process (small mapping count): `/proc/self/statm` **1.5 µs**, `/proc/self/stat` **2.0 µs**, `/proc/self/status` **3.5 µs**, `/proc/self/smaps_rollup` **17 µs**.
  (Measured in this research session; no published per-platform sysinfo overhead numbers were found.)

**Linux /proc**

- statm fields are in pages: size, resident (= VmRSS), shared (= RssFile+RssShmem), plus fields marked broken or always 0. VmRSS = RssAnon + RssFile + RssShmem. — [kernel proc.rst](https://docs.kernel.org/filesystems/proc.html)
- `smaps_rollup` gives accumulated smaps stats across all mappings, including `Pss`, `Pss_Anon`, `Pss_File`, `Pss_Shmem`. It is "faster and more convenient" than deriving the same data from smaps, which has "a significantly higher cost". PSS divides each page by the number of sharers. — [kernel proc.rst](https://docs.kernel.org/filesystems/proc.html)
- RSS counters on SMP are "handled in an asynchronous manner and the value may not be very precise". Reading maps/smaps is "inherently racy". — [kernel proc.rst](https://docs.kernel.org/filesystems/proc.html)

**Windows**

- `PROCESS_MEMORY_COUNTERS_EX.PrivateUsage` is the same as `PagefileUsage`: "The Commit Charge value in bytes for this process. Commit Charge is the total amount of private memory that the memory manager has committed for a running process." On Windows 7/2008 R2 and earlier, `PagefileUsage` is always 0, so use `PrivateUsage`. `WorkingSetSize` is "the current working set size". — [Microsoft Learn PROCESS_MEMORY_COUNTERS_EX](https://learn.microsoft.com/en-us/windows/win32/api/psapi/ns-psapi-process_memory_counters_ex)

**macOS**

- `phys_footprint` is read with `task_info(task, TASK_VM_INFO, …)` into `task_vm_info_data_t`. WebKit uses this as its memory-footprint metric. — [WebKit MemoryFootprintCocoa.cpp](https://webkit.googlesource.com/WebKit/+/0e4328c5b03a383f0100ef33ad31b474fc1d9cab/Source/WTF/wtf/cocoa/MemoryFootprintCocoa.cpp); [WebCore PerformanceLoggingCocoa.mm](https://opensource.apple.com/source/WebCore/WebCore-7606.3.4/page/cocoa/PerformanceLoggingCocoa.mm.auto.html)
- `phys_footprint` includes internal (anonymous) memory, compressed memory, IOKit mappings, purgeable non-volatile memory and page tables. This comes from a quoted XNU comment in a secondary source. — [51CTO blog](https://blog.51cto.com/u_15844020/5792717)
- An Apple Developer Forums report says `resident_size` did not grow during continuous allocation while Xcode's gauge did, and `phys_footprint` tracked Xcode's figure closely. On macOS, compression hides growth from RSS. — [Apple Developer Forums 105088](https://developer.apple.com/forums/thread/105088). Community code suggests Activity Monitor's main memory column ≈ `phys_footprint`. — [Alchemy viewer commit](https://git.alchemyviewer.org/alchemy/viewer/-/commit/bdd76a6a1d0ee0fa5a8da76f1e8c8583822eeabe.diff) (unofficial)

**Industry precedent for the metric**

- Chromium's memory-infra "private memory footprint" (PMF) is built from a per-platform `PlatformPrivateFootprint`: Linux/Android/ChromeOS use `rss_anon_bytes` + `vm_swap_bytes`, macOS uses `phys_footprint` (plus internal/compressed), and Windows uses `private_bytes`. Chromium docs recommend PMF over metrics that "only count the physical ram used". — [Chromium process_memory_dump.cc mirror](https://gitcode.com/openharmony-tpc/chromium_src/blob/master/base/trace_event/process_memory_dump.cc); [Chromium code review 2875823004](https://codereview.chromium.org/2875823004); [Chromium pmf_utils.h](https://chromium.googlesource.com/chromium/src/+/a3dafb8/base/android/pmf_utils.h); [Chromium docs/memory](https://chromium.googlesource.com/chromium/src/+/refs/tags/63.0.3207.1/docs/memory/)
- Chromium's memory key-concepts doc names the sources of truth: VMMap on Windows, `vmmap` on Darwin, `/proc/<pid>/smaps` on Linux. PSS "counts 1/Nth of the resident size", so it changes when other processes map the same library. — [Chromium key_concepts.md](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/memory/key_concepts.md)

### Inferences

- **Recommended sampler design**
  1. Every ~5 s, call `refresh_processes_specifics(ProcessesToUpdate::Some(&tree), true, ProcessRefreshKind::nothing().with_cpu().with_memory().without_tasks())`. Its cost is roughly 7–15 µs × PIDs on Linux, so a tree of 20 PIDs is at most 0.3 ms, about 0.006% of one core at 5 s.
  2. Every 30–60 s (or on spawn events, if the app spawns children itself), rediscover the tree with `ProcessesToUpdate::All` + `ProcessRefreshKind::nothing().without_tasks()` and walk `parent()` from the root PID. The `.without_tasks()` path is about 7 ms on a busy Linux box. The tasks-included `nothing()` path took 22 ms, so always set `.without_tasks()`.
  3. Keep one long-lived `System`. Do not create a new one per tick, because CPU% needs the previous sample.
  4. Run the refresh in `spawn_blocking` or on a dedicated `std::thread`, because sysinfo does synchronous file and syscall I/O.

  Even the 42 ms worst case every 5 s is 0.85% of one core, which is right at the 1% budget, so a full refresh per tick is the one thing to avoid.

- The app owns its children's PIDs when it spawns them (e.g. the provider CLIs), so it can track the tree from spawn/exit events and use the `All` scan only to catch grandchildren.
- macOS: reading another process's `task_info` needs that process's task port. It works for the process itself, but arbitrary children probably need entitlements. Unverified; see Gaps.
- Native leak metric per OS:
  - Linux: read `/proc/<pid>/status` (RssAnon, VmSwap), about 3.5 µs. statm `resident − shared` approximates RssAnon+RssShmem. Use `smaps_rollup` Pss_Anon only on demand, because its cost grows with mapping count.
  - Windows: `GetProcessMemoryInfo` → `PrivateUsage`. sysinfo already exposes this as `virtual_memory()` on Windows only, a quirk that breaks cross-platform consistency.
  - macOS: `task_info(TASK_VM_INFO).phys_footprint` for self. For children, `proc_pid_rusage(RUSAGE_INFO_V2+)` `ri_phys_footprint` is the usual route without the task port (inference; sysinfo already calls `proc_pid_rusage` for disk I/O at `macos/process.rs:812`).
- RSS is a poor leak signal for three reasons. It includes shared file-backed pages (code, mmapped files) that the page cache can evict or refault. It drops under memory pressure or swap, or under macOS compression, while a real leak keeps growing. And on Windows, the working set is trimmed by the OS.
- CPU%: report normalised `cpu_usage()/ncpus` and raw core-percent, and sum over the tree. With a 5 s tick the 200 ms minimum is irrelevant. Discard the first sample after any new PID appears, because it reads 0.

### Gaps

- No published benchmark of sysinfo refresh cost on Windows or macOS was found. Windows cost is dominated by the system-wide Toolhelp snapshot plus per-process `OpenProcess`, so it should be measured on target.
- Whether `proc_pid_rusage` `ri_phys_footprint` works for non-child or child processes without extra entitlements on current macOS was not verified from Apple docs.
- No official Apple documentation defining `phys_footprint` was found. Its definition comes from XNU and WebKit source and community posts.
- Linux `/proc/<pid>/task/<tid>/children` (a cheap way to walk descendants without a full scan) was not verified in this session.

## Q2. Memory-leak detection heuristics on a memory time series

### Takeaway

The strongest production evidence is Microsoft Azure's RESIN (OSDI '22): 5-minute samples, hourly aggregation, robust handling of outlier jumps and drops, a "moving suspicious interval" with enough increasing points, and a regression R² check over the most recent 6 hours. It reported 7 false positives in 291 resolved cases over 14 months. For a single-host desktop/server app, the practical recipe is:

- Use the private-footprint metric.
- Downsample 5 s samples to 1-minute medians.
- Ignore a warm-up period.
- Require a long window (≥ 1–2 h, preferably 6 h) in which the Theil–Sen slope exceeds an absolute MB/h floor and a relative %/h floor, a Mann–Kendall test is significant, and the series sets new rolling-minimum highs (the floor rises, not just the peaks).

That last condition is what defeats allocator retention and GC sawtooth.

### Cited Findings

**RESIN (Azure, production for about 3 years)**

- Four stages: low-overhead detection, live heap snapshots, diagnosis, and mitigation. It files about 24 leak tickets per month and cut low-memory VM reboots 41×. — [USENIX OSDI '22 RESIN](https://www.usenix.org/conference/osdi22/presentation/lou-resin)
- The monitoring agent runs every **5 minutes**, and data is aggregated to **hourly** granularity to reduce noise and volume, "because leaks typically occur in a large time scale". Leaks are "fail-slow faults that last days, weeks, to even months". "Using static thresholds would generate many false positives". Standard anomaly detection fails because "memory usage spikes that are not leaks but legitimate increases" are common. — [RESIN paper PDF](https://www.usenix.org/system/files/osdi22-lou-resin.pdf) §4.1–4.2
- Fleet-level detection groups usage into **20 buckets** by (process image, service). It uses the oldest **2/3** of the window as baseline and the most recent 1/3 as test, and flags an anomaly when the test exceeds **µ + 3σ** of the baseline, using cumulative bucket counts. — [RESIN PDF](https://www.usenix.org/system/files/osdi22-lou-resin.pdf) §4.3.1
- Per-process detection uses a "moving suspicious interval" (Algorithm 1). It keeps [T0, T1] and resets T0 when any of three things happens: a step Δ is a 3σ outlier under a Gaussian model of Δ trained on the last month, the current usage falls below usage at T0, or the fraction of increasing points `Ninc/n < ε`. Severity = ΔU/σ1 + Ninc/n + ΔT(months) + 1/(1+e^(−U/µ1)). — [RESIN PDF](https://www.usenix.org/system/files/osdi22-lou-resin.pdf) §4.3.2
- In 51 real leak cases, growth patterns fell into three types. **Steady** (63%) is near-linear. **Stair** has alternating growth and flat segments. **Spike** is a few large allocations. RESIN classifies the pattern with linear regression on the growing segments after removing flat segments. Over the **most recent six hours** it checks: steady when R² > 0.8; stair when |1 − slope_j/slope_k| < 0.1 and R² < 0.1; spike when ΔU_j/ΔU_k > 0.5. — [RESIN PDF](https://www.usenix.org/system/files/osdi22-lou-resin.pdf) §5.3, Table 1
- Accuracy: **7 false positives out of 291 resolved cases in 14 months**. Both false-positive patterns were legitimate step-ups after a new feature or a configuration change, which then "stabilize and become new baselines". Only **4** false negatives were found. — [RESIN PDF](https://www.usenix.org/system/files/osdi22-lou-resin.pdf) §7

**Other trend methods**

- Applying the Mann–Kendall test to the trend component of a decomposed memory series detects a significant consistent upward trend (a leak) with low runtime overhead. — [IEEE ISSREW 2013](https://doi.ieeecomputersociety.org/10.1109/ISSREW.2013.6688880) (abstract only)
- A Microsoft patent on self-tuning leak detection uses the least-squares slope over a period, with slope > 0 meaning a possible leak. It requires the threshold to be crossed several times before flagging, and notes that plain slope tests break down on real data. — [US 9104563](https://patents.justia.com/patent/9104563)
- A comparison of cloud leak detectors (LBR, LBRCPD, Precog, PrecogMF) on 60 manually labelled VMs, using only host memory utilisation, found PrecogMF best at 85% accuracy with 80% less compute than LBR. Several algorithms wrongly predicted leaks when an application used memory steadily for normal processing. — [arXiv 2106.08938](https://arxiv.org/abs/2106.08938); search summary of [PDF](https://arxiv.org/pdf/2106.08938)
- Theil–Sen (the median of pairwise slopes) has a breakdown point of 1 − 1/√2 ≈ **29.3%**. Naive computation is O(n²) (O(n log n) algorithms exist). It is far less outlier-sensitive than OLS, works well for skewed or heteroskedastic data, and is linked to Kendall's τ. — [Wikipedia Theil–Sen](https://en.wikipedia.org/wiki/Theil%E2%80%93Sen_estimator)

**Allocator retention (why RSS can look like a leak)**

- jemalloc defaults: `opt.dirty_decay_ms`: "The default decay time is 10 seconds". `opt.muzzy_decay_ms`: "disabled by default (with decay time 0)". `opt.narenas`: "four times the number of CPUs". `opt.background_thread`: "disabled by default". — [jemalloc(3)](https://jemalloc.net/jemalloc.3.html)
- jemalloc's tuning guide says shorter decay purges unused pages faster at the cost of more CPU, and more arenas can increase fragmentation. — [jemalloc TUNING.md (Fuchsia mirror)](https://fuchsia.googlesource.com/third_party/github.com/jemalloc/jemalloc/+/f5e840bbf0213d86ae3d0a915df8abd03d75cdf6/TUNING.md)
- glibc: since 2.8, `malloc_trim` iterates over all arenas. Holes in the middle of in-use chunks are not reclaimed by normal trimming. A glibc bug report (2.35) describes containers OOM-killed despite `malloc_trim`, and the workaround was limiting arenas. — [man-pages malloc_trim change](https://kernel.googlesource.com/pub/scm/docs/man-pages/man-pages/+/0deebbe52415e2c85d53902613e51a7b01ea867a); [OpenJDK JDK-8293114 thread](https://mail.openjdk.org/pipermail/hotspot-dev/2023-July/076451.html); [sourceware bug 30769](https://sourceware.org/bugzilla/show_bug.cgi?id=30769)
- A Rust crate documents "monotonic RSS growth" from glibc arenas full of freed-but-not-returned pages. A 16-core box can create up to 128 arenas, and the fix was `M_ARENA_MAX = 2`. — [vct-core heap.rs](https://docs.rs/crate/vct-core/2.2.3/source/src/utils/heap.rs) (secondary)

### Inferences

- **Recommended detector** (single host, per process and per tree total, metric = private footprint):
  1. **Downsample** 5 s samples to 1-minute medians. The median removes spikes and stays cheap: a ring buffer of 360 points covers 6 h at 1-minute resolution, or 720 points for 12 h.
  2. **Warm-up**: ignore the first 10–15 minutes after process start, or after a large known event such as opening a project or a provider restart. Set the baseline to the 1-minute median at the end of warm-up.
  3. **Floor-tracking signal**: use a rolling minimum over 5–10-minute windows (the "floor"). GC sawtooth (JS heap in a WebView or Node child) and allocator burst-then-retain raise peaks without raising the floor. Run trend tests on the floor series as well as the median.
  4. **Trend tests** over the window W (minimum 60 min, preferably 2–6 h):
     - The Theil–Sen slope on 1-minute points is O(n²) ≈ 65k pairs (n(n−1)/2) for n = 360, which is trivial every few minutes. Sampling pairs also works.
     - The Mann–Kendall S statistic must be significant (Z > 2.33, one-sided p < 0.01).
     - The fraction of increasing steps (RESIN's Ninc/n) must exceed about 0.5.
     - The current floor must be higher than the floor at the window start by an absolute minimum.
  5. **Thresholds** (no published per-app standard, so these are engineering defaults to tune): flag **"suspect"** when the slope is ≥ max(**10 MB/h**, **2% of baseline per hour**) sustained over ≥ **1 h**. Flag **"likely leak"** when the slope is ≥ 5 MB/h sustained over ≥ **6 h** with RESIN-style R² > 0.8 on the growing segments, or when growth since baseline is ≥ 50% and ≥ 200 MB. Report the time until a configured ceiling is hit, i.e. (limit − current)/slope.
  6. **False-positive controls** taken from RESIN:
     - Reset the interval start on a 3σ step change. Legitimate step-ups (a new feature, opening a big project) then become a new baseline instead of a trend.
     - Reset when usage drops below the interval start.
     - Require persistence: two or three consecutive evaluations, like the patent's "crosses threshold several times".
     - Rate-limit alerts, e.g. one per process per 6 h.
  7. Never alarm on CPU-only spikes or RSS-only growth when private footprint is flat. That combination is page cache or file mappings.
- With jemalloc's 10 s dirty decay, retained-page noise settles within minutes, so 1-minute medians plus a 10–15-minute warm-up absorb it. glibc malloc can produce a permanent staircase that looks like a leak but plateaus. Requiring growth across ≥ 1 h with a high fraction of increasing points, and resetting on plateaus (as RESIN's stair pattern handling does), separates the two cases. If the server uses glibc malloc, report the allocator in the telemetry header so analysts can discount retention.
- For JS heaps (the web UI in a WebView), use the post-GC floor only, e.g. `performance.memory` or `measureUserAgentSpecificMemory`-like signals where available. Raw heap size is sawtooth by design.

### Gaps

- Specific detection algorithms for Chromium, Firefox telemetry, Meta, Netflix, Datadog Watchdog and LeakCanary were not found from primary sources in this session.
  - Chromium: only its metric choice (PMF) was verified.
  - LeakCanary: it detects leaks by tracking expected-to-be-garbage-collected objects, not from time series (not verified this session).
  - Datadog Watchdog: its "memory leak" detector details were not found.
- No published industry-standard MB/hour thresholds were found. The numbers in Inferences are engineering defaults to tune.
- The IEEE ISSREW 2013 Mann–Kendall paper was read only at abstract level, so its window sizes are unknown.
- The default glibc arena cap (8 × cores on 64-bit) is consistent with the "128 arenas on 16 cores" secondary source, but it was not verified against glibc docs in this session.

## Q3. Non-blocking telemetry writing in Rust (channel, writer, rotation, fsync, NDJSON vs SQLite)

### Takeaway

Hot paths call `tx.try_send(event)` on a bounded `tokio::sync::mpsc` channel (capacity of about 1–4k events) and increment an `AtomicU64` dropped counter on `Full`. They never `await` and never block. A single writer, either a dedicated `std::thread` or one long-running `spawn_blocking`, drains in batches into a `std::io::BufWriter<File>` (64 KiB). It flushes every 1–5 s or when the batch is empty. It does size-based rotation itself, renaming at N MB and keeping K files. It does **not** fsync per event (at most on rotation or shutdown). `tracing-appender` has no size rotation, and `file-rotate` 0.8 does it but is a plain synchronous `Write`. Choose NDJSON over SQLite for append-only telemetry.

### Cited Findings

- `tokio::sync::mpsc::Sender::try_send` is a non-async method. It returns `TrySendError::Full` when the channel is at capacity and `Closed` when the receiver is gone, and gives the message back in the error. There is also `try_reserve` for permit-based sends. — source `tokio-1.53.1/src/sync/mpsc/bounded.rs:924, 1356`; [docs.rs tokio mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Sender.html#method.try_send)
- Tokio's `fs` module performs "ordinary blocking file operations behind the scenes", using the `spawn_blocking` threadpool on all platforms. It "has serious" performance costs per operation, and it recommends batching operations into as few `spawn_blocking` calls as possible and using `BufWriter`. Alternatively, it suggests manually using `std::fs` inside one `spawn_blocking`. — source `tokio-1.53.1/src/fs/mod.rs` module docs (lines 10–204); [docs.rs tokio::fs](https://docs.rs/tokio/latest/tokio/fs/index.html)
- `tracing-appender` file rotation offers only `MINUTELY`, `HOURLY`, `DAILY` (and never). Size-based rotation is not supported. Upstream proposals (tokio-rs/tracing#2497, #2904, #2972) were open or unmerged as of 2024. — [docs.rs tracing-appender](https://docs.rs/tracing-appender); [stackabletech/operator-rs PR 814](https://github.com/stackabletech/operator-rs/pull/814)
- `tracing-appender`'s `RollingFileAppender` does not rotate log files until the first entry is written (#2937), according to the `tracing-logrotate` authors. — [docs.rs tracing-logrotate](https://docs.rs/tracing-logrotate)
- `file-rotate` 0.8.0 is a `std::io::Write` object with `ContentLimit::Bytes`/`Lines` (and time-based) rotation, `AppendCount` or `AppendTimestamp` suffixes, `FileLimit::MaxFiles`/`Age` retention, and optional gzip via flate2. If the log directory becomes inaccessible it keeps running and discards data. The docs do not state whether writes are buffered. — [docs.rs file-rotate](https://docs.rs/file-rotate/latest/file_rotate/)

### Inferences

- **Concrete design**:
  - **Channel**: `mpsc::channel::<TelemetryEvent>(2048)`. Events are pre-built small structs, not pre-serialised strings, so allocation cost stays off the hot path. Serialisation happens in the writer.
  - **Hot path**: `if tx.try_send(ev).is_err() { DROPPED.fetch_add(1, Relaxed) }`, which is a few tens of ns with no await. The sampler itself is not a hot path, but request and stream handlers are.
  - **Writer**: one dedicated `std::thread`, or `spawn_blocking` running a loop with `rx.blocking_recv()`. Drain with `recv_many` (`rx.blocking_recv_many(&mut buf, 256)`, or `try_recv` until empty). Serialise with `serde_json::to_writer(&mut bufwriter, &ev)` + `b'\n'`. Flush when the batch drains and at least 1 s has passed, or on a 5 s timer.
  - **Dropped events**: emit a synthetic `{"type":"telemetry_dropped","count":N}` event whenever the dropped counter changes, so data loss shows up in the file itself.
  - **Rotation**: track bytes written. At ≥ 10 MiB, flush, close, rename `x.ndjson` → `x.1.ndjson` (shifting up to K = 5), and reopen. This is about 30 lines of std code. A crate is not needed, and it avoids `file-rotate`'s "silently discard on error" behaviour, which should be surfaced instead. Rename is atomic on Linux and macOS. On Windows, rename fails if another process holds the file open without `FILE_SHARE_DELETE`, so retry or skip rotation on error.
  - **fsync**: none per write. Call `sync_data()` on rotation and graceful shutdown only. Telemetry tolerates losing the last ≤ 5 s on power loss, and per-event fsync costs milliseconds on consumer SSDs and would make the writer the bottleneck.
  - **Shutdown**: drop the senders, let the writer drain, then flush and sync. Bound the drain time (e.g. 2 s) so it cannot hang shutdown.
- **Overhead**: at one sample event per 5 s plus occasional anomaly events, volume is about 17k events/day × ~300 B ≈ 5 MB/day. At that rate, the writer cost is noise compared with the sampler.
- **NDJSON vs SQLite**:
  - NDJSON is append-only with one `write` per batch and no schema migrations. It is crash-tolerant: at worst the last line is torn, and the reader skips unparseable final lines. It is greppable and trivially shipped or attached to bug reports.
  - SQLite gives queryability (time-range queries for a UI chart), but each commit is a journal/WAL write with fsync by default (`synchronous=FULL`; `NORMAL` in WAL mode syncs only at checkpoint). It needs batching in transactions to reach comparable throughput, and it adds write contention if the app already uses SQLite for persistence.
  - Recommendation: NDJSON for the raw stream. Keep the in-memory ring buffer (6–12 h of 1-minute points) for the live UI and detector, so the file never needs querying at runtime.

### Gaps

- No published benchmark numbers for tokio `try_send` latency or `BufWriter` NDJSON throughput were collected in this session. The "tens of ns" figure is an estimate, not a measurement.
- SQLite fsync and WAL behaviour is stated from general knowledge and was not re-verified against sqlite.org this session.
- The status of the tracing-appender size-rotation PRs after 2024 was not re-checked against the current tokio-rs/tracing main branch.

## Q4. Overhead budgets used by continuous-profiling products

### Takeaway

Published budgets cluster at "a few percent max", with real fleet-wide costs far lower because of sampling. Google-Wide Profiling's aggregated overhead is < 0.01% (per-machine profiling capped at "less than a few percent"). Pyroscope's archived docs say ~2–5% CPU for in-process sampling at ~100 Hz. eBPF profilers like Parca are reported at < 1% per node by third parties. Datadog publishes only qualitative claims ("negligible"). A 5 s CPU/RSS sampler should therefore target ≤ 0.1% CPU, an order of magnitude under the 1% budget. The measurements in Q1 show this is achievable.

### Cited Findings

- GWP: "application owners won't tolerate latency degradations of more than a few percent". Maximum event rates are set "conservatively … to ensure the overhead is always less than a few percent". Whole call stacks are skipped for machine-wide profiles, and symbolisation is offloaded to other machines. "As a result, the aggregated profiling overhead is negligible—less than 0.01 percent." The collector "ceases profiling if the failure rate reaches a predefined threshold". — [Ren et al., Google-Wide Profiling, IEEE Micro 2010](https://research.google/pubs/pub36575/) ([PDF](https://Www.Gwern.net/doc/cs/hardware/2010-ren.pdf))
- The same paper cites Morph's system-wide sampling at "approximately 0.3 percent" overhead. — [GWP PDF](https://Www.Gwern.net/doc/cs/hardware/2010-ren.pdf)
- Pyroscope (archived v1.10 docs): "minimal overhead (~2-5% depending on a few factors)". The current client docs give ~100 Hz stack sampling, a 15 s upload interval, and memory overhead "usually less than 50 MB per pod". — [Grafana Pyroscope v1.10 intro](https://archive.grafana.com/docs/pyroscope/v1.10.x/introduction/continuous-profiling/); [Pyroscope memory overhead](https://grafana.com/docs/pyroscope/next/configure-client/memory-overhead/)
- Datadog Continuous Profiler claims "negligible overhead", and its Ruby profiler uses sampling for "minimal overhead". No percentage was found in the docs. — [Datadog Continuous Profiler](https://datadoghq.com/product/code-profiling); [Datadog press release 2020](https://datadoghq.com/about/latest-news/press-releases/continuous-profiler)
- Parca (eBPF): third-party articles report "< 1%" and "0.5–1% CPU per node". These are not official project numbers. — [jacar.es Parca](https://jacar.es/en/ebpf-profiling-continuo/)
- A third-party 2026 guide lists Pyroscope at ~1–3% CPU (no benchmark cited). — [OneUptime blog](https://oneuptime.com/blog/post/2026-01-30-continuous-profiling-setup/view)

### Inferences

- Budget the profiler like GWP:
  - Set a hard self-budget of ≤ 0.5% of one core. Measure the profiler's own CPU, its thread time and the sampler's elapsed time, and emit it in each sample event.
  - Back off automatically: double the interval or skip tree rediscovery if a tick takes > 50 ms or self-CPU exceeds the budget.
  - Stop after repeated failures, mirroring GWP's "ceases profiling if failure rate" rule.
- From the Q1 numbers on Linux: targeted refresh of ~20 PIDs (~0.3 ms) + a `without_tasks` rediscovery every 60 s (~7 ms → 0.12 ms/s averaged) + NDJSON writes is about **0.01–0.02% of one core**. A per-tick full `refresh_processes(All)` would be about 0.85%, alone consuming nearly the whole 1% budget.

### Gaps

- No official Datadog, Parca or Pyroscope overhead benchmark with methodology was found. The figures are marketing, archived docs or third-party claims.
- No Windows or macOS overhead figures were measured for the sampler. The Windows Toolhelp snapshot cost per tick should be measured on target hardware.
