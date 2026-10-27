//! Test-only settings that keep provider probing, provider update checks, and
//! hosting-CLI discovery confined to fixtures.
//!
//! No test may resolve `claude`, `codex`, `opencode`, `cursor-agent`, or
//! `grok` from the host's real `PATH`, and no test may reach the network for
//! a version/update check. See `docs/testing/README.md`.
//!
//! Integration binaries include this file with
//! `#[path = "support/hermetic_providers.rs"]`; the library's unit tests
//! include it through `src/test_support`, where its own regression test runs
//! once (see `test_support::tests`) instead of once per integration binary.
//!
//! This file is generic over `std::path`/`serde_json` only (no `crate::` or
//! `coddy_studio_server::` paths), because it is compiled both as part of the
//! `coddy_studio_server` lib crate (via `src/test_support/mod.rs`) and as part of
//! every integration test binary that includes it; those two contexts
//! disagree about what the crate is named from this file's point of view.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

/// Drivers back-filled from legacy settings by the server's provider inventory.
/// Keep this list checked against the server's settings and inventory by
/// `test_support::tests::hermetic_helper_pins_every_provider_driver_the_server_defines_as_built_in`,
/// which fails if a driver is added or removed without updating this list.
pub(crate) const BUILTIN_PROVIDER_DRIVERS: &[&str] = &[
    "codex",
    "claudeAgent",
    "coddy",
    "cursor",
    "grok",
    "opencode",
];

/// An absolute, absent path in a test-owned sandbox. Executable resolution
/// must not fall back to PATH when this path is missing.
pub(crate) fn missing_provider_executable(sandbox: &Path) -> PathBuf {
    assert!(sandbox.is_absolute(), "hermetic sandbox must be absolute");
    let missing = sandbox.join("missing-provider-executable");
    assert!(!missing.exists(), "hermetic executable must remain absent");
    missing
}

/// Discovery and lookup seams use this absent directory for hosting CLIs.
/// Git itself remains the real executable so repository assertions stay intact.
pub(crate) fn missing_hosting_executable_dir(sandbox: &Path) -> PathBuf {
    assert!(sandbox.is_absolute(), "hosting sandbox must be absolute");
    let directory = sandbox.join("missing-hosting-bin");
    assert!(
        !directory.exists(),
        "hosting fixture directory must remain absent"
    );
    directory
}

/// Settings that explicitly preserve the built-in enabled defaults (Codex,
/// Claude, Cursor, and OpenCode enabled; Grok disabled) in both settings readers,
/// while pinning each legacy `binaryPath` to [`missing_provider_executable`] and
/// disabling the update check that would otherwise call registry.npmjs.org /
/// downloads.claude.ai.
///
/// A test that needs a driver disabled instead (irrelevant to what it
/// exercises) or a specific `providerInstances` fixture should overlay that
/// through [`write_hermetic_settings`] rather than skipping this base.
pub(crate) fn hermetic_provider_settings(sandbox: &Path) -> Value {
    let missing = missing_provider_executable(sandbox);
    let missing = missing.to_str().expect("sandbox path is valid UTF-8");
    let mut providers = Map::new();
    // The lib guard checks this table's keys against BUILTIN_PROVIDER_DRIVERS
    // and its enabled values against the helper-free control defaults.
    for (driver, enabled_by_default) in [
        ("codex", true),
        ("claudeAgent", true),
        ("coddy", true),
        ("cursor", true),
        ("grok", false),
        ("opencode", true),
    ] {
        providers.insert(
            driver.to_owned(),
            json!({ "enabled": enabled_by_default, "binaryPath": missing }),
        );
    }
    json!({
        "enableProviderUpdateChecks": false,
        "providers": Value::Object(providers),
    })
}

/// Deep-merges `overlay` over [`hermetic_provider_settings`] (overlay wins,
/// recursively; the inverse of production's `merge_missing`, which only fills
/// in what is absent) and writes the result to `state_dir/settings.json`,
/// creating `state_dir` if needed. Returns the settings path.
///
/// `state_dir` must be the exact directory `ServerConfig::state_dir()`
/// resolves for the test under construction (`<base>/userdata`, or
/// `<base>/dev` when a dev URL is set).
pub(crate) fn write_hermetic_settings(state_dir: &Path, overlay: Value) -> PathBuf {
    let mut settings = hermetic_provider_settings(state_dir);
    merge_overlay(&mut settings, overlay);
    std::fs::create_dir_all(state_dir).expect("create hermetic state directory");
    let settings_path = state_dir.join("settings.json");
    std::fs::write(
        &settings_path,
        serde_json::to_vec_pretty(&settings).expect("encode hermetic settings"),
    )
    .expect("write hermetic settings");
    settings_path
}

/// Merges existing `settings.json` over the hermetic base, or writes that base
/// when the file is absent. Shared boot helpers can preserve explicit fixture
/// overrides while pinning unspecified drivers and disabling update checks.
/// Existing files must contain valid JSON; read and parse errors fail the test.
pub(crate) fn ensure_hermetic_settings(state_dir: &Path) -> PathBuf {
    let settings_path = state_dir.join("settings.json");
    let overlay = match std::fs::read(&settings_path) {
        Ok(contents) => serde_json::from_slice(&contents).expect("parse existing fixture settings"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(error) => panic!(
            "read existing fixture settings {}: {error}",
            settings_path.display()
        ),
    };
    write_hermetic_settings(state_dir, overlay)
}

fn merge_overlay(target: &mut Value, overlay: Value) {
    match (target, overlay) {
        (Value::Object(target), Value::Object(overlay)) => {
            for (key, value) in overlay {
                match target.get_mut(&key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        merge_overlay(existing, value);
                    }
                    _ => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, overlay) => *target = overlay,
    }
}

/// Home directory beneath a test-owned sandbox, which may also contain the
/// working directory. Pass it as both HOME and USERPROFILE in the terminal's
/// `env`, never globally.
pub(crate) fn isolated_terminal_home(sandbox: &Path) -> PathBuf {
    assert!(sandbox.is_absolute(), "terminal sandbox must be absolute");
    let home = sandbox.join("terminal-home");
    std::fs::create_dir_all(&home).expect("isolated terminal HOME");
    home
}
