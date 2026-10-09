pub mod event;
pub mod redact;
pub mod store;

use std::path::{Path, PathBuf};

pub fn profiler_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("profiler")
}
