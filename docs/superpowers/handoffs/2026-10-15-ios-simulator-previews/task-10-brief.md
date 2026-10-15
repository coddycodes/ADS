### Task 10: Profiler, docs, runbooks, changelog and native validation

**Files:**
- Modify:
  - `device/mod.rs` (`record_lifecycle` gains `platform`);
  - `docs/skills/bibcode-profiler/references/events.md`;
  - `docs/architecture/overview.md` (Device previews and Profiler sections);
  - `docs/user/workspace-ui.md`;
  - `docs/testing/macos-desktop.md` and `docs/testing/cross-platform-validation.md`;
  - `CHANGELOG.md`;
  - `apps/server/tests/ios_simulator.rs` (finish the harness).

**Interfaces:**
- Consumes: everything above.

- [ ] **Step 1: Write a failing test for the lifecycle event.** The test (in `device/mod.rs`, with the profiler enabled through the existing test hook used by other `profiler::emit` tests):
```rust
#[tokio::test]
async fn lifecycle_events_name_the_platform() {
    let _profiler = crate::diagnostics::profiler::testing::enable();
    let ios = FakeBackend::new(Platform::Ios, &["UDID-1"]);
    let (manager, _enabled) = fake_manager(vec![ios]);
    let id = manager.open(Platform::Ios, "UDID-1").await.unwrap();
    wait_state(&manager, &id, DeviceState::Ready).await;
    let events = crate::diagnostics::profiler::testing::events("device.lifecycle");
    assert!(events.iter().all(|e| e["platform"] == "ios"));
    assert!(events.iter().all(|e| e.get("imageId").is_none()), "never names the image");
}
```
If the profiler has no `testing` module, use whatever capture the existing profiler event tests use; find it with `rg -n "fn .*emit" apps/server/src/diagnostics/profiler`.
- [ ] **Step 2: Implement** the `platform` field. Run the test and expect PASS.
- [ ] **Step 3: Update the docs.**
  - **Profiler events:** `device.lifecycle` `platform` (`"android" | "ios"`) in `events.md` and the overview's Profiler section, plus the `ios-simulator` process identity and its crash reports.
  - **Architecture overview:** the backend split, the iOS backend, the private-API guard and view-only mode.
  - **`workspace-ui.md`:** iOS in the pane, single touch, the paste prompt, and view-only mode.
  - **Runbooks:**
    - `macos-desktop.md` gets an "iOS Simulator previews" procedure: open, live video, tap, type, Home, rotate (if supported), close, setting off, quit, then check `xcrun simctl list` shows the simulator `Shutdown`. It also runs `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator`.
    - `cross-platform-validation.md` adds iOS to the device previews section.
    - `execution-report-template.md`: review it, and change it only if a new evidence field is needed (`iOS sim tests: pass/fail`).
  - **`CHANGELOG.md`:** add `## [Unreleased]` above `## [v0.9.3]` with a "Device previews" section. It has one entry for Android emulators (with H.264 video and E2EE) and one for iOS simulators on macOS servers (single touch, view-only fallback). Write it in the existing changelog voice (user-facing, plain).
- [ ] **Step 4: Run the full validation.** Record each result in the task report.
  - `cargo fmt --all --check`
  - `cargo clippy -p bibcode-server --all-targets -- -D warnings` and `cargo clippy -p bibcode-desktop --all-targets -- -D warnings`
  - `cargo test -p bibcode-server --lib` and `cargo test -p bibcode-server --tests`. The known macOS failures are `provider_runtime an_exited_provider_process_is_read_without_reaping_it` and the three `server_terminal::input_admission_tests`. Report them as pre-existing.
  - `cargo test -p bibcode-desktop`
  - `BIBCODE_IOS_SIM_TESTS=1 cargo test -p bibcode-server --test ios_simulator`
  - `vp check`, `vp run typecheck`, `vp test` (the ChatView "keeps the persisted terminal activity scope pinned…" failure is pre-existing on main), `vp run check:contracts`, `vp run check:dependency-ledger`
- [ ] **Step 5: Native validation on this Mac.**
  1. Build the desktop app the way `docs/testing/macos-desktop.md` says.
  2. Turn on Device previews.
  3. Open the iPhone 17 Pro, check live video, tap Settings, type in Safari's address bar, press Home, rotate (if supported), close, then turn the setting off with a device running and quit with one running.
  4. After each, `xcrun simctl list devices | grep 308FD3CD` must show `Shutdown`, and `pgrep -fl 308FD3CD` must show no BibCode-started process.
  5. Save screenshots and logs in the scratchpad, and summarize them in the task report using the execution report template's fields.
- [ ] **Step 6: Commit** `docs: iOS simulator previews in the overview, runbooks and changelog (#70)`.

---

## Self-review notes (plan author)

**Spec coverage:**

| Spec section | Task |
|---|---|
| §0 spikes | 1 |
| §1 backend split | 2, 3 |
| §2 contracts | 3, 4 |
| §3 lifecycle | 5 |
| §4 video | 6 (JPEG), 7 (H.264) |
| §5 input | 8 |
| §6 guard | 5 (Xcode), 6 (private) |
| §7 client | 4 (shape), 9 (UI) |
| §8 errors | 3 (busy, limit), 5 (boot), 8 (input) |
| §9 profiler | 5 (`report_exit`, attribution), 10 (event, docs) |
| §10 dependencies | 6 |
| §11 testing | each task, plus 10 |
| §12 documentation | 10 |

Out-of-scope items have no task.

**Deliberate deviations from the spec, recorded for the reviewer:**
- **`check_available`.** `DeviceBackend` gains `check_available` (Task 5), so a busy simulator is refused synchronously by `device.open`, as spec §8 requires.
- **MCP `device_open` default.** `platform` defaults to `"android"` when omitted (Task 4). This is a documented input default, not an alias.
- **iOS screenshot format.** MCP `device_screenshot` returns JPEG for iOS when PNG encoding isn't available without a new crate (Tasks 5 and 6).
