use std::sync::Mutex;

#[cfg_attr(test, allow(dead_code))]
pub mod commands;
#[cfg_attr(test, allow(dead_code))]
pub mod host;
pub mod platform;
pub mod registry;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub mod socks;

#[allow(unused_imports)]
pub use registry::{PendingBounds, PreviewRegistry, TabEntry, webview_label_for_tab};

#[allow(dead_code)]
pub struct PreviewHostState(pub Mutex<PreviewRegistry>);

impl PreviewHostState {
    pub fn new() -> Self {
        Self(Mutex::new(PreviewRegistry::new()))
    }
}
