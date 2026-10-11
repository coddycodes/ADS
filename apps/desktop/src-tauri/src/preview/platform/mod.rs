use std::time::Duration;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub use macos::MacosWebviewOps as Platform;
#[cfg(target_os = "macos")]
pub use macos::remove_data_store;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use windows::WindowsWebviewOps as Platform;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub use linux::LinuxWebviewOps as Platform;

/// Places a preview webview at its bounds (logical, relative to the main
/// webview). Only Linux needs more than `set_position`/`set_size`: see
/// `linux::place_child`.
pub fn place_child(webview: &tauri::Webview, x: f64, y: f64, width: f64, height: f64) {
    #[cfg(target_os = "linux")]
    linux::place_child(webview, x, y, width, height);
    #[cfg(not(target_os = "linux"))]
    let _ = (webview, x, y, width, height);
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
pub struct ClearDataKinds {
    pub cookies: bool,
    pub cache: bool,
    pub storage: bool,
}

#[allow(dead_code)]
#[derive(Debug, thiserror::Error)]
pub enum PreviewPlatformError {
    #[error("preview platform unavailable: {0}")]
    Unavailable(String),
    #[error("preview platform call timed out")]
    Timeout,
    #[error("preview javascript error: {0}")]
    Js(String),
}

#[allow(dead_code)]
pub trait PlatformWebviewOps {
    fn eval_json(
        webview: &tauri::Webview,
        js: &str,
        timeout: Duration,
    ) -> Result<String, PreviewPlatformError>;
    fn title(webview: &tauri::Webview) -> Result<String, PreviewPlatformError>;
    fn can_go_back(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError>;
    fn can_go_forward(webview: &tauri::Webview) -> Result<bool, PreviewPlatformError>;
    fn go_back(webview: &tauri::Webview) -> Result<(), PreviewPlatformError>;
    fn go_forward(webview: &tauri::Webview) -> Result<(), PreviewPlatformError>;
    fn hard_reload(webview: &tauri::Webview) -> Result<(), PreviewPlatformError>;
    fn screenshot_png(
        webview: &tauri::Webview,
        timeout: Duration,
    ) -> Result<Vec<u8>, PreviewPlatformError>;
    fn clear_data(
        webview: &tauri::Webview,
        kinds: ClearDataKinds,
    ) -> Result<(), PreviewPlatformError>;
}

/// Wrap arbitrary JS so the completion value is always a JSON envelope.
#[allow(dead_code)]
pub fn json_envelope(js: &str) -> String {
    format!(
        "(function(){{ try {{ const value = (function(){{ return ({js}); }})(); if (value === undefined) {{ return '{{\"ok\":null}}'; }} const serialized = JSON.stringify(value); if (serialized === undefined) {{ throw new Error('result is not JSON-serializable'); }} return '{{\"ok\":' + serialized + '}}'; }} catch (e) {{ return JSON.stringify({{ err: String((e && e.message) || e) }}); }} }})()"
    )
}

#[cfg(test)]
mod tests {
    use super::json_envelope;

    #[test]
    fn envelope_maps_undefined_to_explicit_null() {
        let js = json_envelope("undefined");
        assert!(js.contains("const value = (function(){ return (undefined); })()"));
        assert!(js.contains("if (value === undefined)"));
        assert!(js.contains("return '{\"ok\":null}'"));
    }

    #[test]
    fn envelope_serializes_strings_before_assembling_ok() {
        let js = json_envelope("'value'");
        let serialize = js.find("JSON.stringify(value)").unwrap();
        let assemble = js.find("return '{\"ok\":' + serialized + '}'").unwrap();
        assert!(js.contains("return ('value')"));
        assert!(serialize < assemble);
    }

    #[test]
    fn envelope_serializes_objects_before_assembling_ok() {
        let js = json_envelope("({ answer: 42 })");
        assert!(js.contains("return (({ answer: 42 }))"));
        assert!(js.contains("const serialized = JSON.stringify(value)"));
        assert!(js.contains("return '{\"ok\":' + serialized + '}'"));
    }

    #[test]
    fn envelope_maps_expression_and_serialization_errors_to_err() {
        let js = json_envelope("(() => { throw new Error('boom'); })()");
        assert!(js.contains("throw new Error('boom')"));
        assert!(js.contains("throw new Error('result is not JSON-serializable')"));
        assert!(js.contains("JSON.stringify({ err: String((e && e.message) || e) })"));
    }

    #[test]
    fn envelope_rejects_top_level_non_serializable_results() {
        let js = json_envelope("() => 1");
        assert!(js.contains("if (serialized === undefined)"));
        assert!(js.contains("throw new Error('result is not JSON-serializable')"));
    }
}
