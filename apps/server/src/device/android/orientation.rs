//! Which way a running emulator's screen really faces.
//!
//! The emulator's `SkinRotation` follows the virtual sensor, not the guest: when the screen
//! on display refuses a turn (the Pixel launcher refuses landscape, phones refuse 180°, and
//! apps may lock their orientation), the skin still reports the requested orientation.
//! While a scrcpy session runs, its display size (the codec header, then each session
//! packet) is the guest's real display, so it decides portrait versus landscape and the
//! skin only picks the exact orientation within that class. With no session (still
//! images), the skin stays authoritative: the gRPC screenshots are rotated by it, so a
//! skin-based mapping matches the picture shown.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tokio::sync::watch;

use crate::device::catalog::Orientation;

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_landscape(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Landscape | Orientation::ReverseLandscape
    )
}

/// The orientation to show and map touches with, from the skin's `skin`, the running scrcpy
/// session's display size (`None` without one), and the orientation shown until now.
pub(crate) fn derive_orientation(
    skin: Orientation,
    display: Option<(u32, u32)>,
    previous: Orientation,
) -> Orientation {
    let Some((width, height)) = display else {
        return skin;
    };
    let wide = width > height;
    if is_landscape(skin) == wide {
        // The skin is right whenever the guest turned.
        skin
    } else if is_landscape(previous) == wide {
        // The guest refused the turn the sensor made.
        previous
    } else {
        // The guest turned on its own (an app that forces an orientation); the size cannot
        // tell 90 from 270 degrees.
        let fallback = if wide {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        };
        tracing::debug!(
            ?skin,
            ?previous,
            width,
            height,
            ?fallback,
            "device display disagrees with its skin and its last orientation"
        );
        fallback
    }
}

#[derive(Debug)]
struct State {
    skin: Orientation,
    /// The running scrcpy session and its display size.
    display: Option<(u64, (u32, u32))>,
    next_session: u64,
}

/// A running emulator's skin, scrcpy display, and the orientation derived from them. Every
/// reading that may change the orientation goes through it, so the catalog, touches, and
/// [`RunningDevice::orientation_changes`](crate::device::backend::RunningDevice) agree.
pub(crate) struct OrientationTracker {
    state: Mutex<State>,
    derived: watch::Sender<Orientation>,
}

impl OrientationTracker {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                skin: Orientation::Portrait,
                display: None,
                next_session: 0,
            }),
            derived: watch::Sender::new(Orientation::Portrait),
        })
    }

    /// The orientation shown now.
    pub(crate) fn current(&self) -> Orientation {
        *self.derived.borrow()
    }

    /// Each derived orientation, as it changes.
    pub(crate) fn subscribe(&self) -> watch::Receiver<Orientation> {
        self.derived.subscribe()
    }

    /// A skin rotation the emulator reported; returns the orientation it leads to.
    pub(crate) fn report_skin(&self, skin: Orientation) -> Orientation {
        let mut state = lock(&self.state);
        state.skin = skin;
        self.derive(&state)
    }

    /// The display size of a scrcpy session that starts now; the handle updates it and
    /// clears it when dropped, with the session.
    pub(crate) fn start_display(self: &Arc<Self>) -> DisplayHandle {
        let mut state = lock(&self.state);
        let session = state.next_session;
        state.next_session += 1;
        DisplayHandle {
            tracker: self.clone(),
            session,
        }
    }

    fn set_display(&self, session: u64, size: Option<(u32, u32)>) {
        let mut state = lock(&self.state);
        match size {
            Some(size) => state.display = Some((session, size)),
            // Only the session that set it clears it; a newer one may already run.
            None if state.display.is_some_and(|(owner, _)| owner == session) => {
                state.display = None;
            }
            None => return,
        }
        self.derive(&state);
    }

    /// Derives under the state lock, so concurrent readings publish in order.
    fn derive(&self, state: &State) -> Orientation {
        let previous = *self.derived.borrow();
        let derived = derive_orientation(state.skin, state.display.map(|(_, size)| size), previous);
        self.derived.send_if_modified(|current| {
            let changed = *current != derived;
            *current = derived;
            changed
        });
        derived
    }
}

/// One scrcpy session's display size in an [`OrientationTracker`].
pub(crate) struct DisplayHandle {
    tracker: Arc<OrientationTracker>,
    session: u64,
}

impl DisplayHandle {
    /// The session's display is now `width` x `height`.
    pub(crate) fn set(&self, width: u32, height: u32) {
        self.tracker
            .set_display(self.session, Some((width, height)));
    }
}

impl Drop for DisplayHandle {
    fn drop(&mut self) {
        self.tracker.set_display(self.session, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Orientation::{Landscape, Portrait, ReverseLandscape, ReversePortrait};

    const TALL: Option<(u32, u32)> = Some((576, 1280));
    const WIDE: Option<(u32, u32)> = Some((1280, 576));

    #[test]
    fn without_a_scrcpy_session_the_skin_decides() {
        for skin in [Portrait, Landscape, ReversePortrait, ReverseLandscape] {
            assert_eq!(derive_orientation(skin, None, Portrait), skin);
            assert_eq!(derive_orientation(skin, None, Landscape), skin);
        }
    }

    #[test]
    fn a_skin_whose_class_matches_the_display_decides() {
        assert_eq!(derive_orientation(Landscape, WIDE, Portrait), Landscape);
        assert_eq!(
            derive_orientation(ReverseLandscape, WIDE, Landscape),
            ReverseLandscape
        );
        assert_eq!(derive_orientation(Portrait, TALL, Landscape), Portrait);
        assert_eq!(
            derive_orientation(ReversePortrait, TALL, Portrait),
            ReversePortrait
        );
    }

    /// The native run: the guest refused 180 degrees and stayed at `ROTATION_270` (a wide
    /// display) while the skin said `REVERSE_PORTRAIT`; the launcher refuses landscape.
    #[test]
    fn a_refused_turn_keeps_the_previous_orientation() {
        assert_eq!(
            derive_orientation(ReversePortrait, WIDE, ReverseLandscape),
            ReverseLandscape
        );
        assert_eq!(derive_orientation(Landscape, TALL, Portrait), Portrait);
        assert_eq!(
            derive_orientation(ReverseLandscape, TALL, ReversePortrait),
            ReversePortrait
        );
    }

    #[test]
    fn a_turn_the_guest_made_alone_falls_back_by_shape() {
        assert_eq!(derive_orientation(Portrait, WIDE, Portrait), Landscape);
        assert_eq!(
            derive_orientation(ReversePortrait, WIDE, ReversePortrait),
            Landscape
        );
        assert_eq!(derive_orientation(Landscape, TALL, Landscape), Portrait);
        assert_eq!(
            derive_orientation(ReverseLandscape, TALL, ReverseLandscape),
            Portrait
        );
    }

    #[test]
    fn the_tracker_follows_skin_and_display_and_publishes_changes() {
        let tracker = OrientationTracker::new();
        let mut changes = tracker.subscribe();
        let display = tracker.start_display();
        display.set(576, 1280);
        // The launcher refuses landscape: the skin turns, the display stays tall.
        assert_eq!(tracker.report_skin(Landscape), Portrait);
        assert!(!changes.has_changed().unwrap());
        // Chrome turns: the display becomes wide.
        display.set(1280, 576);
        assert_eq!(tracker.current(), Landscape);
        assert!(changes.has_changed().unwrap());
        assert_eq!(*changes.borrow_and_update(), Landscape);
        // The session ends: the skin decides again.
        drop(display);
        assert_eq!(tracker.report_skin(ReverseLandscape), ReverseLandscape);
    }

    #[test]
    fn an_ending_session_never_clears_a_newer_one() {
        let tracker = OrientationTracker::new();
        let old = tracker.start_display();
        old.set(576, 1280);
        let new = tracker.start_display();
        new.set(576, 1280);
        drop(old);
        // The new session's tall display still overrules a landscape skin.
        assert_eq!(tracker.report_skin(Landscape), Portrait);
        drop(new);
        assert_eq!(tracker.report_skin(Landscape), Landscape);
    }
}
