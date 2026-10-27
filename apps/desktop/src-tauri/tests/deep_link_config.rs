//! The coddy-studio:// URL scheme must stay registered in the bundler config and the
//! deep-link plugin permission must stay granted to the main webview.

#[test]
fn tauri_config_registers_the_coddy_studio_deep_link_scheme() {
    let config: serde_json::Value =
        serde_json::from_str(include_str!("../tauri.conf.json")).expect("valid tauri.conf.json");
    let schemes = config
        .pointer("/plugins/deep-link/desktop/schemes")
        .and_then(|value| value.as_array())
        .expect("plugins.deep-link.desktop.schemes present");
    assert_eq!(
        schemes
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>(),
        vec!["coddy-studio"],
    );
}

#[test]
fn default_capability_grants_deep_link_permission() {
    let capability: serde_json::Value =
        serde_json::from_str(include_str!("../capabilities/default.json"))
            .expect("valid default capability");
    let permissions = capability
        .get("permissions")
        .and_then(|value| value.as_array())
        .expect("permissions array present");
    assert!(
        permissions
            .iter()
            .filter_map(|value| value.as_str())
            .any(|permission| permission == "deep-link:default"),
        "deep-link:default permission missing from capabilities/default.json",
    );
}
