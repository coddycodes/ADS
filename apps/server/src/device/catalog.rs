//! Wire-facing catalog types for device previews.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AvailabilityKind {
    Ready,
    SdkMissing,
    AdbMissing,
    NoAvds,
    AccelerationUnavailable,
    XcodeMissing,
    XcodeSetupIncomplete,
    NoSimulators,
    /// Simulators open as screenshots without touch input; the private-API guard reports it.
    XcodeUnsupported,
    /// Live input needs a GUI login; the private-API guard reports it.
    ViewOnlyHeadless,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    pub kind: AvailabilityKind,
    pub message: String,
}

/// The platform a device image runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Android,
    Ios,
}

/// A device a platform can start: an Android AVD or an iOS simulator.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Image {
    /// The AVD name or the simulator UDID.
    pub id: String,
    pub display_name: String,
    /// Already running outside BiBCode; serialized only when true.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub in_use_elsewhere: bool,
}

/// One platform's availability and images.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCatalog {
    pub platform: Platform,
    pub availability: Availability,
    pub images: Vec<Image>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DeviceState {
    Starting,
    Booting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

/// How the device is turned from its natural portrait position, one convention for every
/// platform (iOS is the reference).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Orientation {
    Portrait,
    /// Turned counter-clockwise ("Rotate left"), right edge up: the UI's top lies along the
    /// device's right edge, so a camera cutout or Dynamic Island shows on the picture's left.
    /// Android `ROTATION_90`.
    Landscape,
    /// Turned 180 degrees. Android `ROTATION_180`.
    ReversePortrait,
    /// Turned clockwise, left edge up. Android `ROTATION_270`.
    ReverseLandscape,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSnapshot {
    pub device_id: String,
    pub platform: Platform,
    pub image_id: String,
    pub state: DeviceState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    pub orientation: Orientation,
    /// Whether the device turns; reported by the running device once `Ready`.
    pub can_rotate: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCatalog {
    /// One entry per configured backend, in configuration order.
    pub platforms: Vec<PlatformCatalog>,
    pub devices: Vec<DeviceSnapshot>,
}
