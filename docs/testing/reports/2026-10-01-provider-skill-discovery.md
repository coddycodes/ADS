# Provider skill discovery validation

**Result:** PASS WITH RESIDUAL RISKS.

## Tested revision and environment

- Repository: Coddy Studio; branch `mubeda/main`.
- Base HEAD: `dc5d6d3852572317d0f1fff461b98e69c43ed23a`, with the uncommitted skill-discovery patch.
- Native automated evidence: Linux x86_64, kernel `7.2.5-3-omarchy`.
- Rust/Cargo `1.98.0`, Node `26.10.0`, Vite+ `0.3.0`.
- Provider tests use temporary homes, fake executables, and loopback HTTP fixtures.
  They do not launch installed provider CLIs or modify user skills.

## Diagnosis and behavior covered

The composer previously consumed an environment-wide inventory discovered from
the server's directory. That catalog did not identify the active chat workspace
and did not consistently apply the provider's effective user configuration.
Cursor also omitted nested/symlinked skills and compatibility roots; OpenCode
discarded skill classification and omitted the workspace directory on discovery
requests. Materialized sensitive environment values remained marked redacted,
which caused native launch/discovery to skip them.

The new typed capability query is scoped by environment, instance, workspace,
and configuration revision. Tests cover user-only catalogs without repository
skills, user/project unions, separate concurrent workspaces, configured homes
and Codex shadow homes, Cursor ancestry/symlinks/cycles, Claude native visibility
and aliases, and local/remote OpenCode directory encoding and authentication.
They also cover partial catalogs, invalid paths and unavailable instances,
loading/error/Retry states, draft preservation, native invocation triggers,
request cancellation, abandoned requests, process reaping, and shutdown admission.

## Commands and results

Commands ran from the repository root. `vp` is not on this shell's PATH, so all
Vite+ checks use the repository's local launcher.

| Command                                                                                                                                            | Result                                                         |
| -------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------- |
| `cargo test -p bibcode-server --lib production::provider_inventory -j 2`                                                                           | 39 passed                                                      |
| `cargo test -p bibcode-server --lib production::control -j 2`                                                                                      | 76 passed                                                      |
| `cargo test -p bibcode-server --lib provider::cursor::capabilities -j 2`                                                                           | 3 passed                                                       |
| `cargo test -p bibcode-server --lib provider::opencode::model -j 2`                                                                                | 6 passed                                                       |
| `cargo test -p bibcode-server --lib production::runtime -j 2`                                                                                      | 22 passed                                                      |
| `cargo test -p bibcode-server --test server_settings_domain --test rpc_wire -j 2`                                                                  | 13 settings and 15 wire tests passed                           |
| `cargo fmt --all --check`                                                                                                                          | Passed                                                         |
| `node scripts/run-local-vp.mjs check`                                                                                                              | Passed; 0 errors, 422 existing warnings                        |
| `node scripts/run-local-vp.mjs run typecheck`                                                                                                      | Passed; all 11 workspace tasks                                 |
| `node scripts/run-local-vp.mjs run --filter @bibcode/web build`                                                                                    | Passed                                                         |
| `cargo clean -p bibcode-server -p bibcode-desktop -p bibcode-updater-verifier` followed by `cargo clippy --workspace --all-targets -- -D warnings` | Passed; final alias patch rerun also passed                    |
| `node packages/contracts/scripts/export-rust-rpc-fixtures.ts`                                                                                      | Regenerated 134 methods and 398 fixtures; parity checks passed |
| `git diff --check`                                                                                                                                 | Passed                                                         |

The following combined TypeScript command passed 195 tests in 10 files, including
contract export/parity, query transport recovery, composer interactions, and
compiled React query behavior:

```sh
node scripts/run-local-vp.mjs test run \
  packages/client-runtime/src/state/server.test.ts \
  packages/client-runtime/src/state/queryTransport.test.ts \
  apps/web/src/components/chat/ChatComposer.test.tsx \
  apps/web/src/components/chat/ChatComposer.rerender.test.tsx \
  apps/web/src/components/chat/ComposerCommandMenu.test.tsx \
  apps/web/src/components/chat/composerCapabilities.test.ts \
  packages/contracts/src/rpcRustParity.test.ts \
  packages/contracts/scripts/export-rust-rpc-fixtures.test.ts \
  apps/web/src/state/query.test.ts \
  apps/web/src/state/query.compiled.test.tsx
```

The fixture verification components were run individually. The aggregate
`check:contracts` script's final clean-diff assertion is not a useful gate for
this uncommitted patch because it intentionally adds RPC fixtures.

## Reviews and remaining evidence

- `UI.md` reviewed: existing native triggers remain, missing catalogs have
  loading/Retry feedback, retries retain the draft, and lookup never needs a new
  provider-selection setting.
- Independent code review found Claude visibility/alias and shutdown ownership
  issues; regression coverage and fixes are included.
- The shared cross-platform validation runbook was updated. Linux, Windows,
  macOS, and SSH runbooks were reviewed and remain accurate.
- React review against `vercel-react-best-practices` was **not run**: the skill
  is unavailable in this environment. Ordinary source review, component tests,
  lint, typechecking, and the web production build were run.
- Packaged native visual validation and live terminal/panel catalog comparison
  were **not run**. Windows and macOS native execution remain unverified; Linux
  fixtures are not a substitute for those checks.
- Configuration revisions reuse the existing transport ownership policy: a new
  catalog can wait for an older request's bounded timeout. Stale results cannot
  populate the newly selected context.
- No dependencies, vendored snapshots, user skill files, or persisted skill
  index were added. Changes remain uncommitted.
