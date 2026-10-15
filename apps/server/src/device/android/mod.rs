//! The Android backend: SDK discovery, emulator lifecycle, its gRPC controller, and the
//! scrcpy H.264 source.
pub(crate) mod emulator;
pub(crate) mod grpc;
pub(crate) mod scrcpy;
pub(crate) mod sdk;
#[cfg(test)]
pub(crate) mod testing;
