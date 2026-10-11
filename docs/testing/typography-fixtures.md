# Typography component fixtures

The standalone gallery at `apps/web/visual/typography` imports the current
React components, their real children, production styles and bundled fonts.
Its typed service ports supply test-owned data and refuse host mutations.
It does not start the server or a packaged desktop application.

Build and check the gallery from `apps/web`:

```sh
node ../../scripts/run-local-vp.mjs build --config visual/typography/vite.config.mjs
node ../../scripts/run-local-vp.mjs test run --config visual/typography/vite.config.mjs
```

The capture caller has a hermetic Node check from the repository root:

```sh
node --test apps/web/visual/typography/*.node-test.mjs
```

Browser captures run in GitHub Actions. The capture command serves only its
built gallery on a disposable loopback listener, performs public component
interactions and saves unmodified light/dark PNGs with a manifest. The manifest
records the source commit, module hashes, named owners, observed text, theme,
fonts, viewport and image hash. A failed witness is not an accepted capture.

The startup gate first captures the real Badge in both themes under a short
external process deadline. It uses the runner's installed Chrome and matching
driver, with bounded page/script waits and an early bootstrap record. This
smoke gate does not establish the full changed-component inventory. Complete
that inventory in subsequent capture batches before accepting the issue.

To fill a reviewed gap without repeating accepted fixtures, pass `selected` and
a comma-separated list of catalog IDs after the engine and output directory.
The caller refuses missing or unknown IDs and records the exact selected scope.
For example, the GTK lane selects the affected PR/MR and branch popovers; it
requires both themes with the native Linux WebKit CSS gate. Combine these
selected packets with the full Chromium groups by current module hash.

The root coverage policy treats this development-only directory like desktop
UI automation. Its compiler and typed service aliases run in the dedicated
gallery test lane, enforced by the workflow and coverage-policy check. The
application source must not import these fixture services; the production
coverage roots and 90% thresholds remain unchanged.

WebKitGTK captures explicitly enable the production `data-linux-webkit` CSS
gate after checking the native Linux WebKit user agent. The fixture page has
no native bridge. This proves component painting with that CSS gate, while
packaged desktop initialization and behavior still require the
[Linux desktop runbook](./linux-desktop.md).

Review original PNGs for clipping, readable contrast, labels and controls.
Join every required changed component and text state to both themes before
accepting the typography sweep. Record execution-specific source commits,
Actions runs, artifacts and observations in a report. Do not treat fixture
data, DOM admission or a successful static build as packaged runtime evidence.
