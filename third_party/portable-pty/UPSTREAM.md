# portable-pty provenance

- Crate: `portable-pty`
- Version: `0.9.0`
- Source: `registry+https://github.com/rust-lang/crates.io-index`
- Crates.io checksum:
  `b4a596a2b3d2752d94f51fac2d4a96737b8705dddd311a32b9af47211f08671e`
- Upstream repository: <https://github.com/wezterm/wezterm>

## Imported files

This directory contains only the normalized crates.io `Cargo.toml`, the
upstream `LICENSE.md`, and the build-required source files:

- `src/cmdbuilder.rs`
- `src/lib.rs`
- `src/serial.rs`
- `src/unix.rs`
- `src/win/conpty.rs`
- `src/win/mod.rs`
- `src/win/procthreadattr.rs`
- `src/win/psuedocon.rs`

## Local deviations

The intended source deviations from portable-pty 0.9.0 are limited to:

- Windows ConPTY support for supplying an existing Job Object through
  `PROC_THREAD_ATTRIBUTE_JOB_LIST` at process creation. This closes the
  interval in which a newly created ConPTY child could create descendants
  before Coddy Studio assigned it to a Job.
- Correct propagation of `TerminateProcess` success and failure from the
  Windows child-killer implementations. Upstream 0.9.0 reverses the Win32
  result check and `WinChild::kill` discards the result, which makes bounded
  cleanup unable to distinguish a terminated child from a failed fallback.
- `CommandBuilder::iter_full_env` exposes the complete captured environment as
  `(&OsStr, &OsStr)` pairs. The upstream `iter_full_env_as_str` filters out any
  entry whose name or value is not UTF-8. Coddy Studio's Linux AppImage child policy
  must inspect every effective PTY entry, including non-UTF-8 command-local
  overrides, so the text-only iterator would leave bundled paths behind. The
  accessor adds no environment filtering or AppImage policy to this crate.
- Unix PTY descriptor cleanup marks inherited descriptors close-on-exec instead
  of closing them in `pre_exec`. This preserves Rust's spawn-reporting socket
  until exec succeeds or fails, so a missing executable or interpreter produces
  a spawn error rather than false success followed by a child abort.

## Updating

1. Update the workspace `portable-pty` version.
2. Download and verify that exact crates.io release.
3. Replace `Cargo.toml`, `LICENSE.md`, and `src/` from Cargo's verified registry
   source; do not copy examples, lockfiles, or Cargo registry metadata.
4. Reapply each deviation above unless the new upstream release supplies its
   equivalent. Preserve a complete raw environment iterator when updating the
   command builder; if upstream provides one under another name, migrate the
   server's AppImage adapter and then remove the local accessor.
5. Update the version and checksum above from the workspace `Cargo.lock`.
6. Run the Windows cross-compile harness, target-gated Windows tests, the Linux
   AppImage PTY tests (including non-UTF-8 environment overrides), and all
   repository checks.

Remove this fork and the workspace `[patch.crates-io]` entry once an upstream
portable-pty release provides equivalent at-creation Job-list support, correct
child-killer result propagation, a preserved Unix spawn handshake, and access
to every raw effective environment entry. Retire individual deviations as their equivalents become available;
the Job-list API alone is not sufficient to remove the remaining fixes.
