# Files recovery and MR source selection validation

**Result:** PASS WITH RESIDUAL RISKS. The listed automated gates and isolated
visual checks passed; packaged-app and unavailable skill-review evidence remain
outstanding as detailed below.

## Tested revision and environment

- Repository: Coddy Studio, branch `mubeda/main`.
- HEAD: `dc5d6d3852572317d0f1fff461b98e69c43ed23a`, with the existing skill-discovery,
  target-selection, and terminal-clipboard work preserved in the dirty checkout.
- Host: Omarchy 4.0.4, Linux x86_64, kernel `7.2.5-3-omarchy`.
- Rust/Cargo 1.98.0; Node 26.10.0; pnpm 11.25.0.
- No release, installation, commit, live forge mutation, or private remote
  download was performed.

## Findings and changes

The exact `Unknown directory child index` exception was reproduced against the
installed Pierre tree model. Its path lookup descended through a file as though
it were a directory when refreshed entries changed a path's kind. The same
lookup also failed while reconciling a selected/focused descendant after a
directory became a file. A one-line package patch returns a missing-path result
at that boundary. Both real-model regressions pass and retain surviving tree
state. The dependency version stays at `1.0.0-beta.6`; the patch is recorded in
the workspace manifest, lockfile, and active dependency ledger. There is no
matching configured vendored subtree.

Files notifications discarded native string errors, download URLs were minted
before the native destination picker, and an archive producer error could close
the stream as a successful partial ZIP. The shared download path now chooses a
destination before minting, preserves bounded failure details, logs failure
stages without signed transfer URLs, and propagates archive production errors to
the HTTP body. Failed desktop streams remove their partial files. These defects
are reproduced; the exact cause of the user's original intermittent failure
cannot be established without its logs.

MR/PR creation now has separate source and required target selectors. The source
can be another local or origin-tracking branch without changing the checkout.
Selected local sources use an exact, non-forced origin push without tags;
origin-only sources need no push. Combined commit actions keep their current or
generated source and explain that constraint. Invalid sources and same-branch
requests are rejected before mutation. The dialog previously queried the newest
commit across all branches; suggestions now use the selected source's tip SHA.
Changing source clears the target and preserves edited title/description.

Older servers silently ignored the new branch fields. A default-false
`gitPullRequestBranchSelection` capability now gates the dialog and the mutation
on the same live connection. Missing support gives update guidance and sends no
mutation, including after reconnect. Ordinary commit/push actions remain usable.

## Commands and results

Commands ran at the repository root unless stated otherwise. `vp` is unavailable
on PATH; its repository launcher supplies the same local Vite+ commands.

| Command                                                                                                                                           | Result                                                 |
| ------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| `codegraph sync .`                                                                                                                                | Passed; graph queries used for cross-package diagnosis |
| `node scripts/run-local-vp.mjs check`                                                                                                             | Passed; 0 errors, 421 existing warnings                |
| `node scripts/run-local-vp.mjs run typecheck`                                                                                                     | Passed; all 11 tasks                                   |
| `node scripts/run-local-vp.mjs run --filter @bibcode/web build`                                                                                   | Passed                                                 |
| `node scripts/run-local-vp.mjs run check:dependency-ledger`                                                                                       | Passed; 0 unaccounted entries                          |
| `node scripts/run-local-vp.mjs test run apps/web/src/components/files`                                                                            | 304 tests passed                                       |
| `cargo test -p bibcode-server --lib transfer:: -j 2`                                                                                              | 21 tests passed                                        |
| `cargo test -p bibcode-server --test production_http_routes -j 2`                                                                                 | 10 tests passed                                        |
| `cargo test -p bibcode-desktop --lib bridge::tests::download_ -j 2`                                                                               | 9 tests passed                                         |
| `cargo test -p bibcode-server --lib production::git_vcs -j 2`                                                                                     | 36 tests passed                                        |
| `cargo test -p bibcode-server --test production_git_vcs_rpc --test rpc_wire -j 2`                                                                 | 40 + 15 tests passed                                   |
| `cargo test -p bibcode-server --lib environment_descriptor_advertises -j 2`                                                                       | 3 tests passed                                         |
| `node packages/contracts/scripts/export-rust-rpc-fixtures.ts`                                                                                     | Passed; capability fingerprints regenerated            |
| `node scripts/run-local-vp.mjs test run packages/contracts/src/rpcRustParity.test.ts packages/contracts/scripts/export-rust-rpc-fixtures.test.ts` | 12 tests passed                                        |
| `cargo fmt --all --check`                                                                                                                         | Passed                                                 |
| `cargo clean -p bibcode-server -p bibcode-desktop -p bibcode-updater-verifier`                                                                    | Passed; fresh first-party Clippy compilation           |
| `cargo clippy --workspace --all-targets -- -D warnings`                                                                                           | Passed; warnings denied                                |
| `git diff --check`                                                                                                                                | Passed                                                 |

MR, transport, and contract checks passed 289 tests in eleven files:

```sh
node scripts/run-local-vp.mjs test run \
  apps/web/src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx \
  apps/web/src/components/gitManager/provider/GitManagerPullRequestPanel.logic.test.ts \
  packages/contracts/src/git.test.ts \
  packages/client-runtime/src/state/vcsAction.test.ts \
  apps/web/src/state/sourceControlActions.behavior.test.ts \
  apps/web/src/components/GitActionsControl.test.tsx \
  apps/web/src/components/SourceControlPanel.test.tsx \
  packages/contracts/src/environment.test.ts \
  packages/shared/src/testSupport.test.ts \
  packages/contracts/src/rpcRustParity.test.ts \
  packages/contracts/scripts/export-rust-rpc-fixtures.test.ts
```

From `apps/web`, the package unit lane passed 267 tests in six files. React
component tests use the package's compiled lane; Files helper/model tests retain
their declared Node environment.

```sh
node ../../scripts/run-local-vp.mjs test run --project unit \
  src/components/gitManager/provider/GitManagerCreatePullRequestDialog.test.tsx \
  src/components/GitActionsControl.test.tsx \
  src/components/SourceControlPanel.test.tsx \
  src/components/files/FileBrowserPanel.test.tsx \
  src/components/files/fileTransfers.test.ts \
  src/components/files/useFileTransfers.test.tsx
```

RED runs reproduced the tree invariant, lost native failure text, mint-before-
picker ordering, archive false success, discarded source RPC field, missing
source control, and wrong-branch commit defaults. Hermetic Git fixtures verify
both hosting-provider argument paths, local and remote-only sources, dirty
checkout preservation, upstream configuration, and refusal before mutation.
Client tests cover absent/false support, reconnect downgrade, connection changes
during negotiation, exact source/target forwarding, and ordinary commit/push.
An initial typecheck found an
unsupported test helper and an exact-optional-property mismatch; both were fixed
before the successful run.

## Visual and interaction evidence

An isolated fixture rendered the real dialog, UI primitives, and built CSS with
mocked state/action boundaries. `node /tmp/bibcode-mr-ui-probe/server.mjs` served
the fixture; `python3 /tmp/bibcode-mr-ui-probe/probe.py` drove headless Chromium
through WebDriver. No private server or forge was connected, and no mutation ran.

Light/dark checks at 1200×857 and 390×701 verified editable source/target fields,
required target selection, unchanged checkout after selecting another source,
combined-commit source guidance, disabled old-server creation, and no horizontal
overflow or runtime errors. Review found the old-server explanation initially
below the narrow viewport; moving it beneath the title made it immediately
visible in both themes. Screenshots and results are local artifacts:

- `/tmp/bibcode-mr-ui-probe/results.json`
- `/tmp/bibcode-mr-ui-probe/mr-{desktop,narrow}-{light,dark}-{target-required,selected}.png`
- `/tmp/bibcode-mr-ui-probe/mr-narrow-{light,dark}-unsupported-initial.png`

WebKit functional interaction also passed, but the tiling compositor kept its
viewport at 341×397 despite resize requests. Desktop-sized WebKit visual
validation is **not available**. This fixture evidence is not packaged-app or
native Windows/macOS validation.

## Reviews and remaining evidence

- Independent source-selection review found no remaining UI/client blocker.
  `UI.md` was reviewed for the selectors, disabled explanations, retained drafts,
  and actionable download errors.
- `vercel-react-best-practices` review was **not run**: that skill is unavailable.
  Ordinary React lifecycle review, compiled tests, lint, typecheck, and build ran.
- Shared architecture, integration, observability, and cross-platform validation
  documentation was updated. Linux, Windows, and macOS native runbooks were
  **reviewed and remain accurate**.
- Packaged-app scenarios, native Windows/macOS execution, and live remote/forge
  validation were **not run**. Hermetic Git/HTTP fixtures do not establish those
  results. The user's installed bundle has not been replaced.
- Final diff/status review found no unintended generated files, debug output,
  dependency version changes, or edits under `.repos/` or `.codegraph/`. Earlier
  authorized changes remain uncommitted alongside these fixes.

Local gate logs: `/tmp/bibcode-current-check.log`,
`/tmp/bibcode-current-typecheck.log`, `/tmp/bibcode-current-web-build.log`,
`/tmp/bibcode-current-dependency-ledger.log`, and `/tmp/bibcode-current-clippy.log`.
