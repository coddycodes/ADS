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

/// Gives each of a platform's images a display name no other of them has, so a picker can tell
/// apart two simulators of the same name and OS, or AVDs whose names differ only by `_`. Only
/// names that collide change: each gets ` · ` and a short tag, an AVD's name or, for a
/// simulator, the last four characters of its UDID, which Xcode's Devices window shows in
/// full. Simulators whose UDIDs also end alike get the whole UDID.
pub(crate) fn distinguish_display_names(platform: Platform, images: &mut [Image]) {
    use std::collections::HashMap;

    fn count<'a>(
        keys: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> HashMap<(&'a str, &'a str), usize> {
        let mut counts = HashMap::new();
        for key in keys {
            *counts.entry(key).or_insert(0) += 1;
        }
        counts
    }
    let short_tag = |id: &str| match platform {
        Platform::Android => id.to_owned(),
        Platform::Ios => {
            let start = id.char_indices().rev().nth(3).map_or(0, |(at, _)| at);
            id[start..].to_uppercase()
        }
    };
    let names = count(images.iter().map(|image| (image.display_name.as_str(), "")));
    let tags: Vec<Option<String>> = images
        .iter()
        .map(|image| (names[&(image.display_name.as_str(), "")] > 1).then(|| short_tag(&image.id)))
        .collect();
    let tagged = count(
        images
            .iter()
            .zip(&tags)
            .filter_map(|(image, tag)| Some((image.display_name.as_str(), tag.as_deref()?))),
    );
    let tags: Vec<Option<String>> = images
        .iter()
        .zip(tags.iter())
        .map(|(image, tag)| {
            let tag = tag.as_deref()?;
            Some(if tagged[&(image.display_name.as_str(), tag)] > 1 {
                image.id.clone()
            } else {
                tag.to_owned()
            })
        })
        .collect();
    for (image, tag) in images.iter_mut().zip(tags) {
        if let Some(tag) = tag {
            image.display_name = format!("{} · {tag}", image.display_name);
        }
    }
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

/// How the device is turned from its natural orientation, one convention for every
/// platform (iOS is the reference). `Portrait` is that natural orientation whatever the
/// panel's shape: a tablet whose panel is naturally landscape is `Portrait` at rest.
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

#[cfg(test)]
mod display_name_tests {
    use super::{Image, Platform, distinguish_display_names};

    fn image(id: &str, display_name: &str) -> Image {
        Image {
            id: id.to_owned(),
            display_name: display_name.to_owned(),
            in_use_elsewhere: false,
        }
    }

    fn names(platform: Platform, images: &[(&str, &str)]) -> Vec<String> {
        let mut images: Vec<Image> = images.iter().map(|(id, name)| image(id, name)).collect();
        distinguish_display_names(platform, &mut images);
        images.into_iter().map(|image| image.display_name).collect()
    }

    #[test]
    fn simulators_of_the_same_name_and_os_get_the_end_of_their_udid() {
        assert_eq!(
            names(
                Platform::Ios,
                &[
                    (
                        "6F1C0D2E-9B3A-4C55-8E21-0A7D44B13F2A",
                        "iPhone 17 · iOS 27.0"
                    ),
                    (
                        "A0B1C2D3-E4F5-4A6B-9C8D-7E6F5A4B9c01",
                        "iPhone 17 · iOS 27.0"
                    ),
                    (
                        "11111111-2222-4333-8444-555555555555",
                        "iPhone 17 Pro · iOS 27.0"
                    ),
                ],
            ),
            [
                "iPhone 17 · iOS 27.0 · 3F2A",
                "iPhone 17 · iOS 27.0 · 9C01",
                "iPhone 17 Pro · iOS 27.0",
            ]
        );
    }

    #[test]
    fn udids_that_also_end_alike_are_shown_whole() {
        assert_eq!(
            names(
                Platform::Ios,
                &[
                    ("AAAA-3F2A", "iPhone 17 · iOS 27.0"),
                    ("BBBB-3F2A", "iPhone 17 · iOS 27.0")
                ],
            ),
            [
                "iPhone 17 · iOS 27.0 · AAAA-3F2A",
                "iPhone 17 · iOS 27.0 · BBBB-3F2A"
            ]
        );
    }

    #[test]
    fn avds_whose_names_differ_by_an_underscore_show_their_names() {
        assert_eq!(
            names(
                Platform::Android,
                &[
                    ("Pixel_8", "Pixel 8"),
                    ("Pixel 8", "Pixel 8"),
                    ("Tablet", "Tablet")
                ],
            ),
            ["Pixel 8 · Pixel_8", "Pixel 8 · Pixel 8", "Tablet"]
        );
    }
}
