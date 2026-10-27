# MR target selection and terminal clipboard validation

**Result:** PASS WITH RESIDUAL RISKS.

## Tested revision and environment

- Repository: Coddy Studio, branch `mubeda/main`.
- Base HEAD: `dc5d6d3852572317d0f1fff461b98e69c43ed23a`, with uncommitted fixes.
- The earlier [provider skill discovery patch](2026-10-01-provider-skill-discovery.md) was preserved.
- Native automated host: Linux x86_64, kernel `7.2.5-3-omarchy`.
- Rust/Cargo `1.98.0`, Node `26.10.0`, Vite+ `0.3.0`.
- Clipboard probe: WebKitGTK `2.52.6` and installed xterm `6.0.0`.
- Release artifacts, remote ancestry and deployment were outside this local bug-fix scope.

## Diagnosis and behavior

Source Control previously invoked `create_pr` without a target. The server chose
`default_ref_name`, falling back to `main`, and passed that value to the hosting
provider. The shared review dialog now opens from all creation entry points with
no target selected. Typing alone does not enable creation. Search and pagination
include local and origin-tracking branches. The selected target travels through
the web adapter, client runtime and typed RPC to the existing hosting adapters.
The server rejects missing, blank, malformed and identical source/target branches
before branch/commit/push mutations. Plain Push remains independent.

Tests cover GitHub and GitLab command arguments, server/RPC rejection before
mutation, explicit selection/clearing, repository changes, status refreshes,
paged refs, cancellation, duplicate prevention, and partial combined-action
retry. A retry rereads Git; changed source branches require explicit target
reselection, including when an external checkout precedes the status subscription.

The terminal intercepted Ctrl+C/V, suppressed native events and silently ignored
asynchronous Clipboard API failures. Selecting output also automatically opened
a menu that could steal keyboard focus. The shared terminal viewport now uses
xterm's native copy/paste handlers; selection keeps focus and Add to chat opens
on right-click. Ctrl+C without selection still interrupts. Shifted copy preserves
pending input and reports a failed native copy command.

## Commands and results

Commands ran at the repository root unless otherwise stated. `vp` is unavailable
on PATH, so checks use the repository's local Vite+ launcher.

| Command                                                                                                                                      | Result                                        |
| -------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------- |
| `cargo test -p bibcode-server --lib production::git_vcs::tests -j 2`                                                                         | 20 passed                                     |
| `cargo test -p bibcode-server --test production_git_vcs_rpc --test rpc_wire -j 2`                                                            | 40 Git/VCS and 15 wire tests passed           |
| `node packages/contracts/scripts/export-rust-rpc-fixtures.ts`                                                                                | Regenerated successfully; parity tests passed |
| `cargo fmt --all --check`                                                                                                                    | Passed                                        |
| `node scripts/run-local-vp.mjs check`                                                                                                        | Passed; 0 errors, 422 existing warnings       |
| `node scripts/run-local-vp.mjs run typecheck`                                                                                                | Passed; all 11 workspace tasks                |
| `node scripts/run-local-vp.mjs run --filter @bibcode/web build`                                                                              | Passed                                        |
| `cargo clean -p bibcode-server -p bibcode-desktop -p bibcode-updater-verifier`, then `cargo clippy --workspace --all-targets -- -D warnings` | Passed                                        |
| `git diff --check`                                                                                                                           | Passed                                        |

MR, transport and contract checks: 260 tests passed in 10 files.

```sh
node scripts/run-local-vp.mjs test run \
  apps/web/src/components/SourceControlPanel.test.tsx \
  apps/web/src/components/GitActionsControl.test.tsx \
  apps/web/src/components/gitManager/provider \
  apps/web/src/state/sourceControlActions.behavior.test.ts \
  packages/client-runtime/src/state/vcsAction.test.ts \
  packages/contracts/src/git.test.ts \
  packages/contracts/scripts/export-rust-rpc-fixtures.test.ts \
  packages/contracts/src/rpcRustParity.test.ts
```

From `apps/web`, the compiled React lane passed 44 tests in 3 files:

```sh
node ../../scripts/run-local-vp.mjs test run --project unit \
  src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx \
  src/components/gitManager/provider/GitManagerPullRequestPanel.test.tsx \
  src/components/pullRequests/PullRequestsPanel.test.tsx
```

Terminal checks passed 285 tests in 5 files in each lane:

```sh
# Repository root
node scripts/run-local-vp.mjs test run \
  apps/web/src/components/ThreadTerminalPanel \
  apps/web/src/components/CenterTerminalPanel.test.tsx \
  apps/web/src/keybindings.test.ts

# apps/web
node ../../scripts/run-local-vp.mjs test run --project unit \
  src/components/ThreadTerminalPanel \
  src/components/CenterTerminalPanel.test.tsx \
  src/keybindings.test.ts
```

The disposable native WebKit probe verified Ctrl+C/V, Ctrl+Shift+C/V,
Ctrl+C without selection emitting ETX, and bracketed multiline paste exactly
once with `navigator.clipboard` deliberately unavailable. Local evidence:
`/tmp/bibcode-terminal-clipboard-probe.py` and
`/tmp/bibcode-terminal-clipboard-probe.log`. The probe restored the clipboard
and closed its browser/driver. It is native engine evidence, not packaged-app evidence.

## Reviews and residual risks

- Independent MR review identified paging and partial-retry gaps; fixes and
  regressions are included. Final narrow review found no remaining blocker.
- `UI.md` reviewed for required explicit selection, disabled reasons, preservation
  of work, keyboard behavior and menu focus. Impeccable detectors returned `[]`
  for the changed MR surfaces and terminal component.
- The shared cross-platform runbook was updated. Linux, Windows and macOS native
  runbooks were **reviewed and remain accurate**.
- `vercel-react-best-practices` review was **not run** because that skill is not
  available. Source review, compiled component tests, lint, typecheck and build
  were run.
- Packaged MR/terminal visual validation and native Windows/macOS execution
  were **not run**. Hosting commands used hermetic fixtures; no real MR was
  created or existing MR retargeted.
- Native stacked actions currently emit start and terminal outcomes without
  intermediate phase events. Living documentation now states that limitation;
  retry checks fresh Git state instead of inferring completed steps from phases.
- No dependency or vendored-subtree changes. All fixes remain uncommitted.
