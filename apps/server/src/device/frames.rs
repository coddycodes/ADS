//! JPEG preview frames: one device frame stream per device while anyone watches it.

use std::{
    sync::{Arc, Weak},
    time::Duration,
};

use base64::Engine;
use futures_util::StreamExt as _;
use serde::Serialize;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::{
    DeviceError, DeviceManager, DeviceOpError, Inner,
    backend::{JpegStream, RawJpeg},
    catalog::{DeviceState, Orientation},
    input::oriented_size,
};

pub(crate) const JPEG_MAX_WIDTH: u32 = 720;
pub(crate) const JPEG_QUALITY: u8 = 70;
pub(crate) const JPEG_MIN_INTERVAL: Duration = Duration::from_millis(66); // ≤ 15 fps
/// How long a failed screenshot stream waits before it reopens.
const STREAM_RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JpegFrame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub orientation: Orientation,
    pub jpeg_base64: String,
}

/// The newest frame of a device; `None` until the first one is encoded. The sender closes
/// when the device stops.
pub struct FrameSubscription {
    pub frames: watch::Receiver<Option<JpegFrame>>,
    _guard: SubscriberGuard,
}

/// A device's running frame producer, shared by its subscribers.
pub(super) struct FrameHub {
    frames: watch::Receiver<Option<JpegFrame>>,
    cancel: CancellationToken,
}

impl FrameHub {
    pub(super) fn stop(&self) {
        self.cancel.cancel();
    }
}

/// Stops the producer when the last subscriber drops it.
struct SubscriberGuard {
    /// Always `Some` until dropped.
    hub: Option<Arc<FrameHub>>,
    inner: Weak<Inner>,
    device_id: String,
}

impl Drop for SubscriberGuard {
    fn drop(&mut self) {
        let Some(hub) = self.hub.take() else {
            return;
        };
        let Some(inner) = self.inner.upgrade() else {
            hub.stop();
            return;
        };
        // Subscribing clones the hub under the same lock, and this reference is released
        // before the lock is, so concurrent last subscribers cannot both miss the stop.
        let mut table = inner.table();
        let slot = table.slots.get_mut(&self.device_id);
        let held = slot
            .as_ref()
            .and_then(|slot| slot.frames.as_ref())
            .is_some_and(|slot_hub| Arc::ptr_eq(slot_hub, &hub));
        if Arc::strong_count(&hub) == 1 + usize::from(held) {
            hub.stop();
            if let Some(slot) = slot.filter(|_| held) {
                slot.frames = None;
            }
        }
        drop(hub);
        drop(table);
    }
}

pub(crate) fn encode_rgb888(rgb: &[u8], width: u32, height: u32) -> Result<Vec<u8>, DeviceError> {
    let (w, h) = (u16::try_from(width), u16::try_from(height));
    let (Ok(w), Ok(h)) = (w, h) else {
        return Err(DeviceError::Protocol("frame too large".into()));
    };
    if rgb.len() != width as usize * height as usize * 3 {
        return Err(DeviceError::Protocol("frame size mismatch".into()));
    }
    let mut out = Vec::with_capacity(rgb.len() / 8);
    jpeg_encoder::Encoder::new(&mut out, JPEG_QUALITY)
        .encode(rgb, w, h, jpeg_encoder::ColorType::Rgb)
        .map_err(|error| DeviceError::Protocol(error.to_string()))?;
    Ok(out)
}

impl DeviceManager {
    /// Subscribes to a `Ready` device's JPEG frames, starting its producer for the first
    /// subscriber.
    pub fn subscribe_frames(&self, device_id: &str) -> Result<FrameSubscription, DeviceOpError> {
        let hub = {
            let mut table = self.inner.table();
            let slot = table
                .slots
                .get_mut(device_id)
                .ok_or(DeviceOpError::NotFound)?;
            if slot.running.is_none() || slot.snapshot.state != DeviceState::Ready {
                return Err(DeviceOpError::NotReady(slot.snapshot.state));
            }
            match &slot.frames {
                // A producer that ended on its own dropped its sender: start a new one.
                Some(hub) if !hub.cancel.is_cancelled() && hub.frames.has_changed().is_ok() => {
                    hub.clone()
                }
                _ => {
                    let (sender, frames) = watch::channel(None);
                    let hub = Arc::new(FrameHub {
                        frames,
                        cancel: slot.cancel.child_token(),
                    });
                    slot.frames = Some(hub.clone());
                    self.inner.tasks.spawn(produce(
                        Arc::downgrade(&self.inner),
                        device_id.to_owned(),
                        sender,
                        hub.cancel.clone(),
                    ));
                    hub
                }
            }
        };
        Ok(FrameSubscription {
            frames: hub.frames.clone(),
            _guard: SubscriberGuard {
                hub: Some(hub),
                inner: Arc::downgrade(&self.inner),
                device_id: device_id.to_owned(),
            },
        })
    }
}

/// Streams a device's screen into `frames` until `cancel`, reopening a failed stream every
/// second while the device is `Ready`.
async fn produce(
    inner: Weak<Inner>,
    device_id: String,
    frames: watch::Sender<Option<JpegFrame>>,
    cancel: CancellationToken,
) {
    let mut producer = Producer {
        inner,
        device_id,
        frames,
        seq: 0,
    };
    loop {
        let Some((stream, native)) = (tokio::select! {
            () = cancel.cancelled() => return,
            opened = producer.open() => opened,
        }) else {
            return;
        };
        let ended = match stream {
            Ok(stream) => producer.pump(stream, native, &cancel).await,
            Err(error) => Err(error),
        };
        if cancel.is_cancelled() {
            return;
        }
        match ended {
            Ok(()) => tracing::warn!("device frame stream ended"),
            Err(error) => tracing::warn!(%error, "device frame stream failed"),
        }
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(STREAM_RETRY_DELAY) => {}
        }
    }
}

struct Producer {
    inner: Weak<Inner>,
    device_id: String,
    frames: watch::Sender<Option<JpegFrame>>,
    seq: u64,
}

impl Producer {
    /// Opens the device's frame stream, with its native display size, while it is `Ready`.
    /// The device is held only while the stream opens.
    async fn open(&self) -> Option<(Result<JpegStream, DeviceError>, (u32, u32))> {
        let (device, _) = self.inner.upgrade()?.running(&self.device_id).ok()?;
        let native = device.native_size();
        Some((device.jpeg_stream(JPEG_MAX_WIDTH).await, native))
    }

    /// Publishes each frame of the stream until it ends or `cancel`.
    async fn pump(
        &mut self,
        mut stream: JpegStream,
        native: (u32, u32),
        cancel: &CancellationToken,
    ) -> Result<(), DeviceError> {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return Ok(()),
                frame = stream.next() => match frame {
                    Some(Ok(frame)) => self.publish(frame, native),
                    Some(Err(error)) => return Err(error),
                    None => return Ok(()),
                },
            }
        }
    }

    fn publish(&mut self, frame: RawJpeg, native: (u32, u32)) {
        let RawJpeg {
            jpeg,
            width,
            height,
            orientation,
        } = frame;
        if let Some(inner) = self.inner.upgrade()
            && inner.reported_orientation(&self.device_id) != Some(orientation)
        {
            let (display_width, display_height) = oriented_size(native.0, native.1, orientation);
            inner.set_reported_orientation(
                &self.device_id,
                orientation,
                display_width,
                display_height,
            );
        }
        self.seq += 1;
        self.frames.send_replace(Some(JpegFrame {
            seq: self.seq,
            width,
            height,
            orientation,
            jpeg_base64: base64::engine::general_purpose::STANDARD.encode(jpeg),
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_rgb888_to_jpeg() {
        let rgb = vec![200_u8; 360 * 800 * 3];
        let jpeg = encode_rgb888(&rgb, 360, 800).unwrap();
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8]);
        assert!(jpeg.len() < rgb.len() / 10);
    }

    #[test]
    fn rejects_frames_that_do_not_match_their_size() {
        assert!(encode_rgb888(&[0; 10], 2, 2).is_err());
        assert!(encode_rgb888(&[], 70_000, 0).is_err());
    }
}

#[cfg(all(test, unix))]
mod device_tests {
    use super::*;
    use crate::device::android::{
        grpc::proto::rotation::SkinRotation,
        testing::{ManagerFixture, wait_until},
    };

    /// The frame the producer publishes for a solid `gray` 720x1600 image; encoding is
    /// deterministic.
    fn gray_frame(gray: u8) -> String {
        let jpeg = encode_rgb888(&vec![gray; 720 * 1600 * 3], 720, 1600).unwrap();
        base64::engine::general_purpose::STANDARD.encode(jpeg)
    }

    async fn wait_frame(
        subscription: &mut FrameSubscription,
        done: impl Fn(&JpegFrame) -> bool,
    ) -> JpegFrame {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if let Some(frame) = subscription.frames.borrow_and_update().clone()
                    && done(&frame)
                {
                    return frame;
                }
                subscription
                    .frames
                    .changed()
                    .await
                    .expect("producer running");
            }
        })
        .await
        .expect("frame within 20 s")
    }

    #[tokio::test]
    async fn slow_subscriber_sees_only_latest_frame() {
        let fixture = ManagerFixture::ready_device().await;
        let mut sub = fixture.manager.subscribe_frames(&fixture.id).unwrap();
        for gray in [10, 20, 30, 40] {
            fixture.controller.frame_tx.send((0, 0, gray)).unwrap();
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        // The subscriber never read while frames arrived: it holds one frame, the newest.
        let expected = gray_frame(40);
        let frame = wait_frame(&mut sub, |frame| frame.jpeg_base64 == expected).await;
        assert_eq!((frame.width, frame.height), (720, 1600));
        assert_eq!(frame.orientation, Orientation::Portrait);
        assert!(!sub.frames.has_changed().unwrap());
    }

    #[tokio::test]
    async fn producer_stops_when_last_subscriber_leaves() {
        let fixture = ManagerFixture::ready_device().await;
        let first = fixture.manager.subscribe_frames(&fixture.id).unwrap();
        let second = fixture.manager.subscribe_frames(&fixture.id).unwrap();
        wait_until(|| fixture.controller.active_screenshot_streams() == 1).await;
        drop(first);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(fixture.controller.active_screenshot_streams(), 1);
        drop(second);
        wait_until(|| fixture.controller.active_screenshot_streams() == 0).await;
        // A new subscriber starts a new producer.
        let mut third = fixture.manager.subscribe_frames(&fixture.id).unwrap();
        wait_frame(&mut third, |_| true).await;
        assert_eq!(fixture.controller.active_screenshot_streams(), 1);
    }

    #[tokio::test]
    async fn closing_the_device_stops_its_producer() {
        let fixture = ManagerFixture::ready_device().await;
        let mut sub = fixture.manager.subscribe_frames(&fixture.id).unwrap();
        wait_frame(&mut sub, |_| true).await;
        fixture.manager.close(&fixture.id).await.unwrap();
        // The producer's sender is gone, so a subscriber's stream ends.
        tokio::time::timeout(Duration::from_secs(10), async {
            while sub.frames.changed().await.is_ok() {}
        })
        .await
        .expect("the frame stream ends with the device");
        assert!(matches!(
            fixture.manager.subscribe_frames(&fixture.id),
            Err(DeviceOpError::NotReady(DeviceState::Stopped))
        ));
        fixture.manager.shutdown().await;
    }

    #[tokio::test]
    async fn frame_stream_updates_orientation_and_size() {
        let fixture = ManagerFixture::ready_device().await;
        let mut sub = fixture.manager.subscribe_frames(&fixture.id).unwrap();
        fixture.controller.report_rotation(SkinRotation::Landscape); // fake now serves 2400x1080 frames
        let frame = wait_frame(&mut sub, |frame| {
            frame.orientation == Orientation::Landscape
        })
        .await;
        assert!(frame.width > frame.height);
        let snapshot = fixture.snapshot();
        assert_eq!(snapshot.orientation, Orientation::Landscape);
        assert_eq!((snapshot.width, snapshot.height), (Some(2400), Some(1080)));
    }
}
