//! Notes GDK losing its display before GTK ends the process (#87).
//!
//! When the Wayland compositor or the X server drops the connection, GDK logs one message and
//! calls `_exit(1)`: no signal, no crash dump, no shutdown. A GLib log writer sees that message
//! first and records it in the profiler's session sentinel, so the next start can say why the
//! run ended.

use std::sync::Once;

use gtk::glib;

/// What GDK (GTK 3) logs right before it exits on a lost display.
const DISPLAY_LOSS: [&str; 5] = [
    "dispatching to Wayland display",
    "Error flushing display",
    "Lost connection to Wayland compositor",
    "Error reading events from display",
    "Fatal IO error",
];

fn is_display_loss(domain: Option<&str>, message: &str) -> bool {
    domain == Some("Gdk") && DISPLAY_LOSS.iter().any(|known| message.contains(known))
}

/// Installs the process's GLib log writer, which notes a display loss and then writes every
/// message as GLib's default writer does. Call before GTK starts; later calls do nothing, since
/// GLib takes one writer per process.
pub(crate) fn install() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        glib::log_set_writer_func(|level, fields| {
            let field = |key: &str| {
                fields
                    .iter()
                    .find(|field| field.key() == key)
                    .and_then(|field| field.value_str())
            };
            if let Some(message) = field("MESSAGE")
                && is_display_loss(field("GLIB_DOMAIN"), message)
            {
                coddy_studio_server::diagnostics::profiler::note_exit(
                    "display-lost",
                    Some(message),
                );
            }
            glib::log_writer_default(level, fields)
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gdk_display_loss_messages_are_recognized() {
        for message in [
            "Error 71 (Protocol error) dispatching to Wayland display.",
            "Error flushing display: Broken pipe",
            "Lost connection to Wayland compositor.",
            "Error reading events from display: Connection reset by peer",
            "coddy-studio-desktop: Fatal IO error 11 (Resource temporarily unavailable) on X server :0.",
        ] {
            assert!(is_display_loss(Some("Gdk"), message), "{message}");
        }
        assert!(!is_display_loss(Some("Gdk"), "Unable to load cursor theme"));
        assert!(!is_display_loss(
            Some("Gtk"),
            "Error flushing display: Broken pipe"
        ));
        assert!(!is_display_loss(
            None,
            "Error flushing display: Broken pipe"
        ));
    }
}
