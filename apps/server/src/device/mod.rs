//! Server-hosted Android device previews; see docs/architecture/overview.md#device-previews.
pub mod catalog;
// Discovery is consumed by the DeviceManager task that follows.
#[allow(dead_code)]
pub(crate) mod sdk;
// Launch and stop are consumed by the DeviceManager task that follows.
#[allow(dead_code)]
pub(crate) mod emulator;
pub(crate) mod grpc;
// Handle fields are consumed by the lifecycle, frame, and input tests that follow.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod testing;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum DeviceError {
    #[error("emulator gRPC: {0}")]
    Grpc(String),
    #[error("{0}")]
    Io(String),
    #[error("protocol: {0}")]
    Protocol(String),
}
