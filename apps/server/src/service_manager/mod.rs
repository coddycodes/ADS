//! Per-user background service management for the headless server
//! (`coddy-studio service install | uninstall | status`). Definitions are rendered
//! from a [`ServiceSpec`]; platform service managers are driven through
//! [`CommandRunner`] so tests can assert the exact command sequence.

mod definitions;
mod manager;

pub use definitions::ServiceSpec;
pub use manager::ServiceError;
pub(crate) use manager::{
    ProcessCommandRunner, ServiceLocations, ServicePlatform, install, status, uninstall,
};
