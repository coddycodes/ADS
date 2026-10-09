# OS crash and OOM evidence a Rust desktop/server app can collect on its next start (Linux, Windows, macOS)

Scope: what a Rust process (Tauri desktop host plus in-process Axum server) can read on its next start, with no helper process, to answer two questions: "did my previous run die uncleanly?" and "what does the OS crash record say about it?"

Note on evidence: man7.org and freedesktop.org returned HTTP 403 or 404 to the fetcher. The systemd facts below come from the man pages installed on a test machine running **systemd 261.2 (Arch Linux, kernel 7.2)**. The citations link to the equivalent upstream man pages. Items marked **[Local probe]** were observed directly on that machine (user uid 1000, a member of `wheel`; see the caveat under Q1).

---

## Q1. Linux: systemd-coredump / coredumpctl / journal / apport / core_pattern

### Takeaway

On a systemd-coredump system, the reliable unprivileged path is `coredumpctl --json=short list` (to find the entry) plus `journalctl -a -o json MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1 COREDUMP_PID=<pid>` (to read the full record, including the symbolized backtrace in `MESSAGE`). An unprivileged user normally sees only their own crashes. The core file gets a per-user read ACL. Other machines need a different path: check `/proc/sys/kernel/core_pattern`. If it starts with `|/usr/share/apport/apport`, read Ubuntu apport's `/var/crash/*.crash`. Otherwise (WSL, Alpine, many containers) the OS keeps no record and the app must rely on its own sentinel plus a panic or signal hook.

### Cited Findings

**coredumpctl commands and JSON**

- `--json=MODE` accepts `short`, `pretty` or `off` (the default). `-1` means the most recent entry only. `-n INT` was added in v248. `-S/--since` and `-U/--until` were added in v233. `-r/--reverse`, `-F/--field=`, `-o/--output=`, `--file=GLOB` (v246), `-D DIR` (v225) and `--root=` (v252) are also available. The `list`, `info` and `dump` verbs date from v215, and `debug` from v239. — [coredumpctl(1)](https://man7.org/linux/man-pages/man1/coredumpctl.1.html) (read from the local systemd 261.2 man page)
- `list` columns: TIME, PID, UID/GID, SIGNAL, COREFILE and EXE. COREFILE takes one of these values:
  - `none`: the core was not stored.
  - `-`: no core is available (for example, the process was not terminated by a signal).
  - `present`: the core is accessible by the current user.
  - `journal`: the core was stored in the journal.
  - `truncated`: one of the above, but the core was too large to store in full.
  - `error`: the core cannot be accessed, most likely for lack of permission.
  - `missing`: the core was stored in a file that has since been removed.

  — [coredumpctl(1)](https://man7.org/linux/man-pages/man1/coredumpctl.1.html)

- "coredumpctl needs access to the journal files to retrieve the relevant entries from the journal. Thus, an unprivileged user will normally only see information about crashing programs of this user." — [coredumpctl(1)](https://man7.org/linux/man-pages/man1/coredumpctl.1.html)
- A journal entry can outlive its core file because the journal and `/var/lib/systemd/coredump` have different retention, so an entry with COREFILE=`missing` is normal. — [coredumpctl(1)](https://man7.org/linux/man-pages/man1/coredumpctl.1.html)
- **[Local probe]** `coredumpctl --json=short list --no-pager` emits one JSON array of objects with the keys `time` (µs since the epoch), `pid`, `uid`, `gid`, `sig`, `corefile` (string), `exe` and `size`. Example: `{"time":1790707025243866,"pid":1732134,"uid":1000,"gid":1000,"sig":6,"corefile":"present","exe":"/work/.../bibcode_server-fe771ab9e6f4b1bc","size":739904}`. The key names are lowercase and differ from the column headers. Filtering works by PID, COMM, EXE path, or a journal `FIELD=value` match, for example `coredumpctl --json=short list /path/to/exe --since=-1d`.
- **[Local probe]** `coredumpctl info -1` prints human-readable text: PID, TID, UID, GID, `Signal: 6 (ABRT) si_code: SI_TKILL`, Timestamp, Command Line, Executable, Control Group, Unit, User Unit, Slice, Owner UID, Boot ID, Machine ID, Hostname, `Storage: <path> (present)`, Size on Disk, then `Message:` containing the stack trace. `info` has no stable JSON form, so use journalctl JSON for machine parsing.

**Journal fields (systemd-coredump(8))**

- The relevant messages have `MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1`. The documented example is `journalctl MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1 -o verbose`. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)
- Documented fields:
  - `COREDUMP_PID`, `COREDUMP_UID`, `COREDUMP_GID`. Container values are as seen from outside.
  - `COREDUMP_TIMESTAMP`: "the time of the crash as reported by the kernel (in μs since the epoch)".
  - `COREDUMP_SIGNAL_NAME` and `COREDUMP_SIGNAL`: both are included "because signal numbers vary by architecture".
  - `COREDUMP_CODE`: si_code; added in v261.
  - `COREDUMP_BY_PIDFD=1`: the process was analysed via a kernel pidfd, which requires kernel ≥ 6.16. Otherwise it was analysed via PID, "subject to race conditions". Added in v258.
  - `COREDUMP_DUMPABLE` (v258).
  - `COREDUMP_RLIMIT`.
  - `COREDUMP_UNIT`, `COREDUMP_SLICE`, `COREDUMP_CGROUP`, `COREDUMP_PROC_CGROUP`.
  - `COREDUMP_OWNER_UID`, `COREDUMP_USER_UNIT`, `COREDUMP_SESSION`: user processes only.
  - `COREDUMP_CWD`, `COREDUMP_ROOT`, `COREDUMP_OPEN_FDS`.
  - `COREDUMP_EXE`: "the destination of the /proc/pid/exe symlink".
  - `COREDUMP_CMDLINE`, `COREDUMP_COMM`, `COREDUMP_ENVIRON`, `COREDUMP_PROC_AUXV`, `COREDUMP_PROC_LIMITS`, `COREDUMP_PROC_MAPS`, `COREDUMP_PROC_MOUNTINFO`, `COREDUMP_PROC_STATUS`.
  - `COREDUMP_HOSTNAME`, `COREDUMP_CONTAINER_CMDLINE`.
  - `COREDUMP`: the core itself, when Storage=journal.
  - `COREDUMP_FILENAME`: when stored externally.
  - `COREDUMP_TRUNCATED=1`.
  - `COREDUMP_PACKAGE_NAME`, `COREDUMP_PACKAGE_VERSION`, `COREDUMP_PACKAGE_JSON`: from `.package` ELF notes, v249.
  - `MESSAGE`: "includes the backtrace if it was successfully generated".

  Most of these fields were added in v248. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)

- External core files also carry xattrs that mirror the key fields: `user.coredump.pid`, `.uid`, `.gid`, `.signal`, `.timestamp`, `.rlimit`, `.hostname`, `.comm` and `.exe`. They can be read with getfattr. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)
- journalctl JSON output: "Fields larger than 4096 bytes are encoded as null values. (This may be turned off by passing --all…)". Non-UTF-8 values are encoded as byte arrays, and repeated fields as arrays. — [journalctl(1)](https://man7.org/linux/man-pages/man1/journalctl.1.html)
- **[Local probe]** Without `-a`, `MESSAGE` in the coredump entry came back as `null` (it exceeds 4096 bytes), so **`-a` is mandatory** to get the backtrace. The parser must also accept `MESSAGE` as either a string or an array of byte values.
- **[Local probe]** The backtrace format inside `MESSAGE`:
  ```
  Process 3366838 (turn_delivery_r) of user 1000 dumped core.

  Stack trace of thread 3366839:
  #0  0x00007def25c9a17c n/a (libc.so.6 + 0x9a17c)
  #1  0x00007def25c3e5d0 raise (libc.so.6 + 0x3e5d0)
  #3  0x00005d59ced2baea _RNvNtNtNtCs6ZjlLoI6YmX_3std3sys3pal4unix14abort_internal (turn_delivery_recovery-edb… + 0x58c3aea)
  ```
  Each frame is `#N  0xADDR SYMBOL|n/a (MODULE + 0xOFFSET)`. Rust symbols appear **mangled** (v0 `_R…` here; legacy `_ZN…` is also possible), so pass them through `rustc-demangle`. The module offset allows offline symbolication with `addr2line` or `symbolic` against the matching build. Sections for other threads may follow. `COREDUMP_COMM` is the kernel `comm`, truncated to 15 characters (`turn_delivery_r`). Match on `COREDUMP_EXE`, not on comm.
- **[Local probe]** The full key set observed on systemd 261 also includes `COREDUMP_TID`, `COREDUMP_THREAD_NAME`, `COREDUMP_PIDFDID`, `_BOOT_ID`, `__REALTIME_TIMESTAMP` and `__CURSOR`. `_BOOT_ID` plus `COREDUMP_PID` identify the crashed process instance.

**Permissions**

- "All users are granted access to their private per-user journals. However, by default, only root and users who are members of a few special groups are granted access to the system journal and the journals of other users. Members of the groups 'systemd-journal', 'adm', and 'wheel' can read all journal files." — [journalctl(1)](https://man7.org/linux/man-pages/man1/journalctl.1.html)
- **[Local probe]** `/var/lib/systemd/coredump` is `drwxr-xr-x root root`. Each core file is `-rw-r-----+ root root` with the ACL `user:mauro:r--`, so the crashing user can read their own compressed core (`.zst`) without privilege. The tmpfiles rule is `d /var/lib/systemd/coredump 0755 root root 2w`, which means **cores are aged out after 2 weeks** by systemd-tmpfiles.
- The kernel runs the systemd-coredump handler in privileged mode. It hands off to the socket-activated, unprivileged `systemd-coredump@.service` for processing. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)

**Storage disabled and limits (coredump.conf(5))**

- `Storage=none|external|journal`, with `external` as the default. With `none`, "the core dumps may be logged (including the backtrace if possible), but not stored permanently". The core is still written temporarily for processing unless `ProcessSizeMax=0`. "Setting Storage=none and ProcessSizeMax=0 disables all coredump handling except for a log entry." — [coredump.conf(5)](https://man7.org/linux/man-pages/man5/coredump.conf.5.html)
- `ProcessSizeMax` defaults to 32G on 64-bit systems. Above it the core may be stored but **no stack trace is generated**. `JournalSizeMax` defaults to 767M, which is a hard journal limit. `MaxUse` defaults to 10% of the disk, capped at 4 GiB, and `KeepFree` to 15%, capped at 4 GiB. Old cores are also removed by time via systemd-tmpfiles. — [coredump.conf(5)](https://man7.org/linux/man-pages/man5/coredump.conf.5.html)
- The core is processed only if `RLIMIT_CORE` is sufficient (see core(5)). For services, `LimitCORE=` controls this. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)

**core_pattern**

- systemd installs `/usr/lib/sysctl.d/50-coredump.conf` to set `kernel.core_pattern`, and the setting can be masked or overridden. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)
- **[Local probe]** systemd 261 sets `|/usr/lib/systemd/systemd-coredump %P %u %g %s %t %c %h %d %F %I`. `%F` is the pidfd and `%I` the global TID.
- Kernel rules: if the first character of `core_pattern` is `|`, the rest is run as a command. The pattern can be at most 127 characters, and the default value is `core`. Specifiers include `%p/%P` (PID, global PID), `%i/%I` (TID), `%u/%g`, `%d` (dump mode), `%s` (signal), `%t` (time), `%h`, `%e/%f/%E` (executable name or path), `%c` (RLIMIT_CORE), `%C` (CPU) and `%F` (pidfd). `core_pipe_limit` defaults to 0. — [kernel sysctl docs](https://docs.kernel.org/admin-guide/sysctl/kernel.html)

**apport (Ubuntu)**

- With apport, core_pattern looks like `|/usr/share/apport/apport -p%p -s%s -c%c -d%d -P%P -u%u -g%g -- %E`. Reports go to `/var/crash/`, and `apport-unpack` extracts a `.crash` report. — [lx.iriss core-pattern page](https://www.lx.iriss.org.uk/read/core-pattern.html); [linuxcommandlibrary apport](https://linuxcommandlibrary.com/man/apport) (third-party)
- As of Apport 2.28.0 (Ubuntu 24.04), systemd-coredump can be installed in parallel to Apport. The packages provide, conflict with and replace `core-dump-handler`, so installing systemd-coredump makes APT remove apport. — [Ubuntu Discourse: Apport 2.28.0 gained systemd-coredump integration](https://discourse.ubuntu.com/t/apport-2-28-0-gained-systemd-coredump-integration/44910)

**Containers**

- Full-OS containers can forward cores to their own coredump service via `CoredumpReceive=`. `systemd-nspawn --boot` defaults to this. Host-side processing of namespaced crashes requires `EnterNamespace=yes`, which defaults to off. Container field values are recorded as seen outside the container. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html); [coredump.conf(5)](https://man7.org/linux/man-pages/man5/coredump.conf.5.html)

**WebKitGTK children**

- **[Local probe]** WebKitGTK helper binaries live in `/usr/lib/webkit2gtk-4.1/` (WebKitWebProcess, WebKitNetworkProcess, WebKitGPUProcess) and in `/usr/lib/webkitgtk-6.0/` for GTK4. Debian and Ubuntu use a multiarch path such as `/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/` (not checked here).
- The documented example shows a browser child process: `COREDUMP_COMM=Web Content`, `COREDUMP_EXE=/usr/lib64/firefox/firefox`, `COREDUMP_CMDLINE=... -contentproc -childID 5`, `COREDUMP_USER_UNIT=app-gnome-firefox-552136.scope`. A child is therefore told apart from the main process by EXE/CMDLINE, while it shares the app's scope or cgroup. — [systemd-coredump(8)](https://man7.org/linux/man-pages/man8/systemd-coredump.8.html)

### Inferences

- **Identifying "our" crash.** Persist `{pid, boot_id (/proc/sys/kernel/random/boot_id), exe path (/proc/self/exe), start time}` in the sentinel at startup. On the next start, look for a coredump entry with `COREDUMP_PID == pid && _BOOT_ID == boot_id` (both are journal match fields: `journalctl -a -o json MESSAGE_ID=fc2e… COREDUMP_PID=<pid> _BOOT_ID=<id>`). That entry is the main-process crash. Separately, entries with `COREDUMP_PROC_CGROUP`/`COREDUMP_USER_UNIT` equal to our scope, or `COREDUMP_EXE` under `…/webkit2gtk-4.1/WebKit*Process`, plus a time window, are WebKit child crashes. A WebKit child crash does not kill a Tauri main process, so the main process can observe those live. They also show up in the journal for later harvesting.
- **Shell out or use a library.** Running `journalctl`/`coredumpctl` as a child process is still "no helper process" in the sense of no long-lived daemon. A pure-Rust alternative is reading the journal through `libsystemd` (sd-journal, via the `systemd` crate). That adds a C dependency, but either path works on systemd hosts. If both binaries are absent, skip Linux OS-record harvesting.
- **Skip the core itself.** Never load the core file in-app. The `MESSAGE` backtrace (bounded, already symbolized by elfutils when debuginfo is present) is the useful artifact. Keep only an opt-in path to `coredumpctl dump` for export.
- **The local test was not a pure unprivileged test** because uid 1000 is in `wheel`, so it could read the system journal. The man page claims user journals hold the user's coredump entries, but a test account outside `wheel`/`adm`/`systemd-journal` should confirm that before relying on it.

### Gaps

- Exactly how journald routes coredump entries into the per-user journal file was not verified from source, nor whether it works when journald runs with `Storage=volatile` (no persistent per-user files).
- WSL2 and Alpine (OpenRC, no systemd-coredump by default) core_pattern defaults were not verified from primary sources. Treat them as "no OS record" and detect this at runtime by reading core_pattern.
- The internal format of apport `.crash` files (an RFC822-like key: value layout with base64 CoreDump) was not confirmed from a primary source, nor were their file permissions.

---

## Q2. Linux OOM: kernel OOM killer, /dev/kmsg, cgroup v2 memory.events, systemd-oomd, exit status

### Takeaway

A kernel or oomd kill delivers SIGKILL. SIGKILL produces **no core dump and no coredump journal entry**, so OOM evidence has to come from somewhere else. The kernel log is usually unreadable to unprivileged users (`dmesg_restrict=1` blocks `/dev/kmsg`, and kernel messages live in the system journal). The best unprivileged signal is the cgroup v2 `memory.events` `oom_kill` counter, read from a cgroup that **outlives** the process (for example `user@UID.service` or `app.slice`, since counters are hierarchical) and compared against a snapshot stored at the previous start.

### Cited Findings

- `memory.events` (non-root cgroups) fields:
  - `low`, `high`, `max`
  - `oom`: "the cgroup's memory usage was reached the limit and allocation was about to fail"
  - `oom_kill`: "the number of processes belonging to this cgroup killed by any kind of OOM killer"
  - `oom_group_kill`: "the number of times a group OOM has occurred"
  - `sock_throttled`

  "All fields in this file are hierarchical." `memory.events.local` holds the non-hierarchical equivalents. The `memory_localevents` mount option restores the legacy local-only behaviour. — [Linux kernel cgroup-v2 docs](https://docs.kernel.org/admin-guide/cgroup-v2.html)

- `memory.oom.group=1` makes the OOM killer kill the whole cgroup together. Tasks with `oom_score_adj=-1000` are never killed. — [Linux kernel cgroup-v2 docs](https://docs.kernel.org/admin-guide/cgroup-v2.html)
- `dmesg_restrict=1` requires `CAP_SYSLOG` to use dmesg. The default comes from `CONFIG_SECURITY_DMESG_RESTRICT`. — [kernel sysctl docs](https://docs.kernel.org/admin-guide/sysctl/kernel.html)
- **[Local probe]** `dmesg_restrict` is 1, and `head /dev/kmsg` as uid 1000 fails with "Operation not permitted". `/proc/self/cgroup` is `0::/user.slice/user-1000.slice/user@1000.service/app.slice/<name>.scope`. `/sys/fs/cgroup/user.slice/user-1000.slice/memory.events` is world-readable and contained `low 0 / high 0 / max 334993 / oom 0 / oom_kill 0 / oom_group_kill 0 / sock_throttled 3114`.
- systemd-oomd monitors only cgroups of units with `ManagedOOMSwap=kill` or `ManagedOOMMemoryPressure=kill`, and polls PSI. When it acts, it "will select a cgroup to terminate, and send SIGKILL to all processes in it". Only leaf cgroups, or cgroups with `memory.oom.group=1`, are eligible. The man page suggests a lower pressure limit (for example 40%) for user@.service. — [systemd-oomd.service(8)](https://man7.org/linux/man-pages/man8/systemd-oomd.service.8.html)
- systemd-oomd's journal messages take forms like "Killed <cgroup> due to memory pressure for <slice> being X% > Y% for > 20s with reclaim activity" and "systemd-oomd killed N process(es) in this unit". These come from a Fedora QA test case and an Ubuntu bug report, not a formal spec. — [Fedora QA: Memory Pressure Based Killing](https://fedoraproject.org/wiki/QA:Testcase_Memory_Pressure_Based_Killing); [Launchpad bug 1972159](https://bugs.edge.launchpad.net/ubuntu/+source/systemd/+bug/1972159/comments/50)
- Fedora enables systemd-oomd by default. — [Fedora Changes/EnableSystemdOomd](https://www.fedoraproject.org/wiki/Changes/EnableSystemdOomd)
- **[Local probe]** systemd-oomd is `active` on the test machine (Arch).

### Inferences

- **Algorithm for "was I OOM-killed?" (unprivileged).**
  1. At start, read `/proc/self/cgroup`, walk up to the first ancestor that persists across runs (`user@UID.service`, or `app.slice` under it), and persist `oom_kill` and `oom_group_kill` alongside the sentinel.
  2. On the next start, if the sentinel shows an unclean exit, no coredump entry matches, and `oom_kill` in that ancestor increased, classify the exit as **"likely OOM-killed"**. Another process in the same ancestor could have been the victim, so this is a heuristic. Tighten it by also checking whether the app's own scope name was transient.
  3. The app's own `*.scope` cgroup is removed once empty, so its own `memory.events` cannot be read after death.
- **Supervised children** (the server's provider subprocesses, terminals) are the cheap case. `waitpid`/`ExitStatus::signal() == Some(SIGKILL)` with no request from us is a strong OOM or oomd indication. Reading the child's cgroup `memory.events` is only useful if the child runs in a cgroup we created, which is not the case by default.
- If the user is in `systemd-journal`/`adm`/`wheel`, `journalctl -k -o json --since=<sentinel start> -g 'Out of memory: Killed process <pid>'` gives the kernel OOM record. Without that membership it returns nothing, so treat it as optional enrichment.
- oomd kills are logged by a system service into the system journal, so the same group-membership caveat applies (inference, not verified).

### Gaps

- No primary source was found for the exact kernel OOM message format on current kernels (for example "Out of memory: Killed process PID (comm) total-vm:… anon-rss:…"), nor for its stability across versions.
- The exact systemd-oomd log `MESSAGE_ID` and whether oomd sets xattrs (such as `user.oomd_ooms`/`user.oomd_kill`) on cgroups were not found in the fetched man page.
- The default `ManagedOOMMemoryPressure` setting for `user@.service` on Fedora, Ubuntu and Arch in 2026 was not verified.

---

## Q3. Windows: WER LocalDumps, Event Log 1000/1001

### Takeaway

LocalDumps works only under **HKLM** and needs admin rights to enable, so a per-user app cannot turn it on at runtime. An installer running elevated can create `HKLM\…\LocalDumps\<app>.exe` once. The default dump folder is `%LOCALAPPDATA%\CrashDumps`. Without admin rights, the evidence on next start is the Application event log's **Event 1000 (source "Application Error")** and **1001 (Windows Error Reporting)**. Those events give the faulting module, exception code, fault offset, PID, start time and paths.

### Cited Findings

- LocalDumps has existed since Vista SP1 and Server 2008. "This feature is not enabled by default. Enabling the feature requires administrator privileges." It is configured under `HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\Windows Error Reporting\LocalDumps`, with these values:
  - `DumpFolder`: REG_EXPAND_SZ, default `%LOCALAPPDATA%\CrashDumps`. A non-default folder needs ACLs that let the crashing process write.
  - `DumpCount`: REG_DWORD, default 10. The oldest dump is replaced when the count is exceeded.
  - `DumpType`: 0 custom, 1 mini (the default), 2 full.
  - `CustomDumpFlags`: MINIDUMP_TYPE bits, default 0x121.

  Per-application overrides go under `LocalDumps\MyApplication.exe`. — [Microsoft Learn: Collecting User-Mode Dumps](https://learn.microsoft.com/en-us/windows/win32/wer/collecting-user-mode-dumps)

- "Applications that do their own custom crash reporting are not supported by this feature." No dump is collected when automatic debugging (AeDebug) is configured. Dumps are collected even if WER is disabled or the user cancels reporting. — [Microsoft Learn: Collecting User-Mode Dumps](https://learn.microsoft.com/en-us/windows/win32/wer/collecting-user-mode-dumps)
- WER settings may live under HKCU or HKLM `Software\Microsoft\Windows\Windows Error Reporting`. However, `DumpFolder`, `DumpCount`, `DumpType` and `CustomDumpFlags` each state: "This setting is not supported in the HKEY_CURRENT_USER registry hive." The HKCU hive does support settings such as `DontShowUI`, `Disabled`, `DisableArchive`, `MaxArchiveCount` (1–5000, default 1000) and `MaxQueueCount` (1–500, default 50). — [Microsoft Learn: WER Settings](https://learn.microsoft.com/en-us/windows/win32/wer/wer-settings)
- Microsoft's own procedure also writes the `HKLM\SOFTWARE\Wow6432Node\…\LocalDumps\<exe>` key for 32-bit processes on 64-bit Windows. It uses an elevated `reg add … /v DumpType /t REG_DWORD /d 2`. — [Microsoft Learn: troubleshoot application crashing](https://learn.microsoft.com/en-us/troubleshoot/windows-server/performance/troubleshoot-application-service-crashing-behavior)
- Event 1000 (Log: Application, Source: Application Error, Task Category: Application Crashing Events, Level: Error) contains Faulting application name/version/timestamp, Faulting module name/version/timestamp, Exception code (e.g. `0xc0000005`), Fault offset, Faulting process id, Faulting application start time, application path, module path, Report Id, and package full name / package-relative app ID. Event 1001 accompanies it. Common codes are `0xc0000005` (ACCESS_VIOLATION), `0xc0000374` (HEAP_CORRUPTION) and `0xc0000022` (ACCESS_DENIED). The guidance applies to all supported Windows client and server versions (page dated 2026-02). — [Microsoft Learn: troubleshoot application crashing](https://learn.microsoft.com/en-us/troubleshoot/windows-server/performance/troubleshoot-application-service-crashing-behavior)
- The faulting module is often a victim system DLL (ntdll, kernel32, kernelbase) rather than the culprit. — [Microsoft Learn: troubleshoot application crashing](https://learn.microsoft.com/en-us/troubleshoot/windows-server/performance/troubleshoot-application-service-crashing-behavior)

### Inferences

- **Query to use** (standard XPath on the event log; the syntax is from general knowledge and should be verified on a target machine):
  `wevtutil qe Application /q:"*[System[Provider[@Name='Application Error'] and (EventID=1000) and TimeCreated[timediff(@SystemTime) <= 604800000]]]" /f:xml /rd:true /c:20`.
  In Rust, call `EvtQuery(NULL, L"Application", xpath, EvtQueryChannelPath | EvtQueryReverseDirection)`, then `EvtNext` and `EvtRender(EvtRenderEventXml)` via the `windows` crate (feature `Win32_System_EventLog`). Parse the `<EventData><Data>` elements **positionally**. Their order matches the description lines: app name, app version, app timestamp, module name, module version, module timestamp, exception code, fault offset, PID, process start time, app path, module path, report ID, package full name, package relative app ID. That order should be checked against a real event.
- **Correlation.** Store the PID (as hex) and process creation time in the sentinel. Event 1000 carries "Faulting process id" and "Faulting application start time" (a FILETIME), which together identify the instance exactly. Filter on app path equal to our exe.
- **WebView2 crashes** are reported against `msedgewebview2.exe`, not our exe (inference). Filter Event 1000 for that app name with a module path inside our user-data folder or a matching time window. The WebView2 `ProcessFailed` event is a better live signal.
- If the installer sets `LocalDumps\<app>.exe` with `DumpFolder=%LOCALAPPDATA%\<App>\CrashDumps`, dumps land per-user in a known folder. The naming convention observed in practice is `<exe>.<pid>.dmp` (unverified). The app can then list new `.dmp` files on start. A per-user install with no elevation cannot do this.
- A Rust app that installs its own in-process crash handler (`SetUnhandledExceptionFilter` plus `MiniDumpWriteDump`) falls into "custom crash reporting", which LocalDumps says it does not support. In-process minidump writing from a corrupted process is also unreliable. Decide consciously between relying on WER (Event 1000, optional LocalDumps) and your own handler.

### Gaps

- No primary source was found confirming the default ACL of the Application channel for standard users on Windows 10 and 11. It is widely believed to be readable by authenticated users.
- Locations of WER report archives (`%LOCALAPPDATA%\Microsoft\Windows\WER\ReportArchive`, `%ProgramData%\Microsoft\Windows\WER\ReportArchive`, `Report.wer` format) were not verified from fetched sources.
- The dump file naming pattern in DumpFolder was not confirmed from a primary source.
- Whether `WerRegisterAppLocalDump` or similar per-app APIs exist for unpackaged Win32 apps was not researched.

---

## Q4. macOS: ~/Library/Logs/DiagnosticReports `.ips`

### Takeaway

Since macOS 12, crash reports are `.ips` files made of **two JSON objects**: a one-line metadata object (check `bug_type == "309"` for a crash) followed by the crash body. The body gives `exception.type/signal/codes`, `termination`, `faultingThread`, `threads[].frames[]{imageIndex,imageOffset,symbol,symbolLocation}` and `usedImages[]{base,size,uuid,path,name,arch}`. A frame's runtime address is `usedImages[imageIndex].base + imageOffset`. Match the image `uuid` to our binary or dSYM UUID and symbolicate offline with `atos` or the `symbolic` crate.

### Cited Findings

- "Starting with iOS 15 and macOS 12, crash reports are stored as JSON in files with a `.ips` extension." The file contains two JSON objects: line 1 is the IPS metadata and the remainder is the crash report. Check `bug_type`: `309` is a crash report and `288` a stackshot. — [Apple: Interpreting the JSON format of a crash report](https://developer.apple.com/documentation/xcode/interpreting-the-json-format-of-a-crash-report)
- Metadata keys: `name`, `bug_type`, `bundleID`, `build_version`, `incident_id`, `platform` (1 = macOS) and `timestamp`. Apple says to use the body's `procLaunch` and `captureTime` for the real times. — [Apple: Interpreting the JSON format](https://developer.apple.com/documentation/xcode/interpreting-the-json-format-of-a-crash-report)
- Body keys:
  - `procName`, `procPath` (redacted), `pid`, `parentPid`/`parentProc`
  - `procLaunch`, `captureTime`, `uptime`
  - `osVersion{train,build,releaseType}`, `cpuType`, `translated` (Rosetta)
  - `bundleInfo{CFBundleIdentifier,CFBundleShortVersionString,CFBundleVersion}`
  - `exception{type,signal,codes,subtype,message}`: Mach-level only
  - `termination{code,indicator,namespace,byPid,byProc,flags}`
  - `faultingThread`: index into `threads`
  - `threads[]{id,frames,queue,threadState,triggered}`
  - `frames[]{imageIndex,imageOffset,symbol,symbolLocation}`
  - `usedImages[]{base,size,uuid,path,name,arch,source}`: `source` is P process, S shared cache, and so on
  - `asi`: application-specific info strings
  - `lastExceptionBacktrace`, `isCorpse`, `isNonFatal`, `isSimulated`, `vmSummary`, `vmregioninfo`, `version`

  — [Apple: Interpreting the JSON format](https://developer.apple.com/documentation/xcode/interpreting-the-json-format-of-a-crash-report)

- JSON stores addresses and codes as decimal numbers, so convert them to hex for display. — [Apple: Interpreting the JSON format](https://developer.apple.com/documentation/xcode/interpreting-the-json-format-of-a-crash-report)
- Apple DTS describes the file as "two JSON dictionaries back to back", where "the first is sometimes missing". Parsers must tolerate a missing header. Real files contain undocumented keys such as `os_version`, `app_name`, `slice_uuid` and `app_version`. — [Apple Developer Forums 746848](https://developer.apple.com/forums/thread/746848)
- Reports are in `~/Library/Logs/DiagnosticReports` and are kept only for a limited time, with the oldest deleted first. A `viewdiagnostic` tool exists on macOS 14.4+. — [dmaclach/crashdump](https://github.com/dmaclach/crashdump) (third-party)

### Inferences

- **Harvest algorithm.**
  1. On start with an unclean sentinel, list `~/Library/Logs/DiagnosticReports/*.ips`, plus `Retired/` if present (unverified), with mtime ≥ the sentinel's start time.
  2. Read the first line. If it parses as JSON with `bug_type == "309"`, parse the rest as the body. Otherwise try the whole file as the body.
  3. Match on `pid == sentinel.pid` and `procPath` ending in our exe name, or on `bundleInfo.CFBundleIdentifier`.
  4. Store `exception`, `termination`, `faultingThread`, and the faulting thread's frames with `(usedImages[i].name, uuid, imageOffset)`.
- **Rust symbolication.** `atos -arch arm64 -o <dSYM or binary> -l 0x<base> 0x<base+imageOffset>` works on a dev machine only, since `atos` ships with Xcode CLT. In-app, use the `symbolic` crate with a shipped or downloadable dSYM, keyed by `usedImages[].uuid`. Rust panics with `panic=abort` appear as `EXC_CRASH (SIGABRT)` with `abort` on the faulting thread.
- WKWebView content-process crashes produce their own `.ips` with `procName` `com.apple.WebKit.WebContent` (inference). They do not belong to our app's pid.
- Jetsam or memory-pressure kills on macOS appear as `termination.namespace` values rather than Mach exceptions (inference; not verified).

### Gaps

- No primary Apple source was found for the retention period, the file-naming pattern (`<proc>-YYYY-MM-DD-HHMMSS.ips`) or the `Retired/` subfolder.
- No Apple documentation was found on whether a sandboxed or hardened-runtime app can read `~/Library/Logs/DiagnosticReports`. A sandboxed (Mac App Store) app likely cannot. A Developer ID non-sandboxed Tauri app can.
- The `.ips` exception/termination fields for macOS memory-limit kills were not researched.

---

## Q5. Clean-shutdown sentinel patterns (Chrome, Firefox, VS Code)

### Takeaway

Every major browser uses the same pattern: **at startup, write a beacon saying "not exited cleanly" (durably); on orderly shutdown, write "exited cleanly"; on the next start, read the beacon before resetting it.** None of them can tell a crash from a power loss or OS kill using the beacon alone. They rely on a separate crash reporter (Crashpad) for detail, which is exactly the gap that OS records fill.

### Cited Findings

- Chrome desktop: "During shutdown, Chrome writes a beacon indicating a clean shutdown. At startup, Chrome reads the beacon. If it doesn't indicate a clean last shutdown, the previous shutdown was unclean. During startup, the beacon is reset." Android monitors only the foreground period. Unclean shutdowns are counted in UMA `Stability.Counts2`. Causes include OS crash, power outage, dead battery and force quit, and the beacon cannot distinguish them. Details come from Crashpad. — [Chromium docs/stability.md](https://chromium.googlesource.com/chromium/src/+/HEAD/docs/stability.md)
- Chrome profile `Preferences` contains `exit_type` (for example `"Crashed"` / `"Normal"`) and `exited_cleanly`. The shutdown path writes the preferences to record shutdown status. — [Volumio forum](https://community.volumio.com/t/how-can-remove-chromium-didnt-shut-down-correctly/7552) (community); [Chromium code review 1057533003](https://codereview.chromium.org/1057533003)
- Firefox counts consecutive startup crashes in the pref `toolkit.startup.recent_crashes` and offers Safe Mode after a threshold (`toolkit.startup.max_resumed_crashes`; about 3 per a support volunteer). The logic sits in `nsAppStartup` (`GetAutomaticSafeModeNecessary`). Crash pings carry a `StartupCrash` annotation. — [Bugzilla 1095799](https://bugzilla.mozilla.org/show_bug.cgi?id=1095799); [Bugzilla 726043](https://bugzilla.mozilla.org/show_bug.cgi?id=726043); [Bugzilla 1306013](https://bugzilla.mozilla.org/show_bug.cgi?id=1306013); [SUMO thread](https://support.mozilla.org/mg/questions/1141677) (community)

### Inferences

Recommended sentinel design for BiBCode, synthesized from the sources above:

- **Write it at startup.** `last_run.json` in the app data dir holds `{state:"running", pid, boot_id (Linux /proc/sys/kernel/random/boot_id; Windows/macOS: system boot time), exe_path, app_version, started_at, cgroup_oom_kill_baseline}`. Write it via temp file, fsync, rename, and fsync the directory so it survives power loss.
- **Mark clean exit.** On orderly shutdown, rewrite it with `state:"clean"` and `ended_at`. Do this after the server has drained, not in a Drop that may never run.
- **Classify on next start.**
  - `running` with the same boot ID means **app crash or kill**, so harvest OS records: Linux coredump/journal plus cgroup `oom_kill` delta, Windows Event 1000/LocalDumps, macOS `.ips`.
  - `running` with a different boot ID means **system crash, power loss or reboot without clean exit**. OS app records are unlikely, so report "previous session ended with the system".
  - No match in any OS record, same boot, and no `oom_kill` delta means SIGKILL from a user or task manager, a hang killed by the OS, or an unknown cause.
- **Keep a startup-crash counter** like Firefox's: increment it when the sentinel is still in the "starting" phase. After N consecutive failures, start in a degraded or safe mode (for example skipping session restore).
- **Panic hook.** In-process `std::panic::set_hook` with `panic=abort` can append the panic message and backtrace to the sentinel before abort. That is the richest Rust-native record on every OS, and the OS record then supplies the signal, exception code and native frames. Signal handlers that do I/O are unsafe beyond writing to a preopened fd, so keep it minimal.

### Gaps

- No primary source was found for VS Code's unclean-exit detection. A lock or marker in the user-data dir is suspected but not verified.
- Firefox's exact implementation (`TrackStartupCrashBegin`/`End` in `nsAppStartup.cpp`, and the `.startup-incomplete`/`compatibility.ini` mechanics) was not confirmed from source. Searchfox was not fetched.
- No authoritative documentation was found for Chrome's Local State `exited_cleanly` vs Preferences `exit_type` split or the variations-safe-mode beacon.
