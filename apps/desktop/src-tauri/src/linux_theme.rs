//! AppImages use bundled Adwaita without pinning its light/dark variant through
//! GTK_THEME. Setting gtk-theme-name on GtkSettings has application priority over
//! XSETTINGS/settings.ini and changes only this process (GTK3 Settings docs).
//! Tao owns portal updates; an explicit Coddy Studio choice must survive those updates.

use std::{
    ffi::OsStr,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};

#[cfg(not(test))]
use gtk::gio;
use gtk::{glib::Variant, glib::variant::ToVariant, prelude::GtkSettingsExt};
use tauri::Theme;

#[derive(Default)]
pub struct LinuxThemeState {
    // Serialize read/apply so a slow System request cannot overwrite a
    // later explicit choice. GTK notifications never acquire this async lock.
    pub request_lock: tokio::sync::Mutex<()>,
    requested: Arc<AtomicU8>,
}

impl LinuxThemeState {
    pub fn remember(&self, theme: Option<Theme>) {
        let value = match theme {
            None => 0,
            Some(Theme::Light) => 1,
            Some(Theme::Dark) => 2,
            _ => 0,
        };
        self.requested.store(value, Ordering::Relaxed);
    }
}

fn requested_theme(requested: &AtomicU8) -> Option<Theme> {
    match requested.load(Ordering::Relaxed) {
        1 => Some(Theme::Light),
        2 => Some(Theme::Dark),
        _ => None,
    }
}

pub fn resolve_appimage_theme_override(
    is_appimage: bool,
    gtk_theme: Option<&OsStr>,
    appimage_gtk_theme: Option<&OsStr>,
) -> Option<&'static str> {
    let has_override = [gtk_theme, appimage_gtk_theme]
        .into_iter()
        .flatten()
        .any(|value| !value.is_empty());
    (is_appimage && !has_override).then_some("Adwaita")
}

pub fn resolve_theme_to_apply(requested: Option<Theme>, system: Theme) -> Theme {
    requested.unwrap_or(system)
}

pub(crate) async fn read_system_theme_with(
    read: impl FnOnce() -> Option<Theme> + Send + 'static,
) -> tauri::Result<Theme> {
    tauri::async_runtime::spawn_blocking(move || {
        read().unwrap_or_else(|| {
            tracing::debug!("portal color scheme unavailable; using tao's light fallback");
            Theme::Light
        })
    })
    .await
}

fn theme_from_portal_reply(reply: &Variant) -> Option<Theme> {
    // Legacy Settings.Read wraps the value twice; retain compatibility with
    // version 1 portals, just as tao does, rather than requiring ReadOne.
    if !reply.is::<(Variant,)>() {
        return None;
    }
    let (value,) = reply.get::<(Variant,)>()?;
    if !value.is::<Variant>() {
        return None;
    }
    let scheme = value.as_variant()?.get::<u32>()?;
    Some(if scheme == 1 {
        Theme::Dark
    } else {
        Theme::Light
    })
}

#[cfg(not(test))]
fn read_portal_theme() -> Option<Theme> {
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;
    let reply = connection
        .call_sync(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
            "Read",
            Some(&("org.freedesktop.appearance", "color-scheme").to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            5_000,
            gio::Cancellable::NONE,
        )
        .ok()?;
    theme_from_portal_reply(&reply)
}

#[cfg(not(test))]
pub async fn read_system_theme() -> tauri::Result<Theme> {
    // WebviewWindow::theme() dispatches back to Tauri's main thread, where tao
    // blocks on D-Bus. Read the same setting on a worker instead; tao still owns
    // the live subscription. No GTK object is accessed from this worker.
    read_system_theme_with(read_portal_theme).await
}

pub fn resolve_prefer_dark_override(requested: Option<Theme>, prefer_dark: bool) -> Option<bool> {
    requested
        .map(|theme| theme == Theme::Dark)
        .filter(|requested_dark| *requested_dark != prefer_dark)
}

/// Called once from Tauri setup on the GTK main thread.
pub fn configure_theme(state: &LinuxThemeState) {
    let Some(settings) = gtk::Settings::default() else {
        tracing::debug!("GTK settings are unavailable; skipping Linux theme setup");
        return;
    };

    if let Some(theme_name) = resolve_appimage_theme_override(
        std::env::var_os("APPIMAGE").is_some(),
        std::env::var_os("GTK_THEME").as_deref(),
        std::env::var_os("APPIMAGE_GTK_THEME").as_deref(),
    ) {
        settings.set_gtk_theme_name(Some(theme_name));
    }

    let requested = Arc::clone(&state.requested);
    settings.connect_gtk_application_prefer_dark_theme_notify(move |settings| {
        if let Some(prefer_dark) = resolve_prefer_dark_override(
            requested_theme(&requested),
            settings.is_gtk_application_prefer_dark_theme(),
        ) {
            // The resulting notification sees the requested value and is a no-op.
            settings.set_gtk_application_prefer_dark_theme(prefer_dark);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portal_reply(scheme: u32) -> Variant {
        // The legacy Settings.Read method returns two variant wrappers.
        (Variant::from_variant(&scheme.to_variant()),).to_variant()
    }

    #[test]
    fn portal_read_decodes_double_wrapped_color_scheme() {
        assert_eq!(theme_from_portal_reply(&portal_reply(1)), Some(Theme::Dark));
        for scheme in [0, 2, 99] {
            assert_eq!(
                theme_from_portal_reply(&portal_reply(scheme)),
                Some(Theme::Light)
            );
        }
    }

    #[test]
    fn malformed_portal_replies_have_no_theme() {
        for reply in [
            1_u32.to_variant(),
            ("dark",).to_variant(),
            (1_u32.to_variant(),).to_variant(),
            (Variant::from_variant(&"dark".to_variant()),).to_variant(),
        ] {
            assert_eq!(theme_from_portal_reply(&reply), None);
        }
    }

    #[tokio::test]
    async fn portal_read_runs_off_the_command_thread() {
        let command_thread = std::thread::current().id();
        let theme = read_system_theme_with(move || {
            assert_ne!(std::thread::current().id(), command_thread);
            Some(Theme::Dark)
        })
        .await
        .expect("portal read worker");
        assert_eq!(theme, Theme::Dark);
    }

    #[tokio::test]
    async fn unavailable_portal_keeps_taos_light_fallback() {
        assert_eq!(
            read_system_theme_with(|| None)
                .await
                .expect("portal read worker"),
            Theme::Light
        );
    }

    #[test]
    fn appimage_uses_adwaita_without_a_user_override() {
        assert_eq!(
            resolve_appimage_theme_override(true, None, None),
            Some("Adwaita")
        );
        assert_eq!(
            resolve_appimage_theme_override(true, Some(OsStr::new("")), Some(OsStr::new(""))),
            Some("Adwaita")
        );
    }

    #[test]
    fn appimage_preserves_either_user_theme_override() {
        for (gtk_theme, appimage_theme) in [
            (Some("Custom:dark"), None),
            (None, Some("Adwaita:light")),
            (Some("Custom:dark"), Some("Adwaita:light")),
            (Some(""), Some("Adwaita:dark")),
            (Some("Custom:light"), Some("")),
        ] {
            assert_eq!(
                resolve_appimage_theme_override(
                    true,
                    gtk_theme.map(OsStr::new),
                    appimage_theme.map(OsStr::new)
                ),
                None
            );
        }
    }

    #[test]
    fn ordinary_linux_launch_keeps_the_session_theme_name() {
        assert_eq!(resolve_appimage_theme_override(false, None, None), None);
    }

    #[test]
    fn system_theme_resolves_both_portal_color_schemes() {
        assert_eq!(resolve_theme_to_apply(None, Theme::Dark), Theme::Dark);
        assert_eq!(resolve_theme_to_apply(None, Theme::Light), Theme::Light);
    }

    #[test]
    fn explicit_theme_ignores_the_portal_color_scheme() {
        for system in [Theme::Dark, Theme::Light] {
            assert_eq!(
                resolve_theme_to_apply(Some(Theme::Dark), system),
                Theme::Dark
            );
            assert_eq!(
                resolve_theme_to_apply(Some(Theme::Light), system),
                Theme::Light
            );
        }
    }

    #[test]
    fn system_never_reasserts_prefer_dark() {
        assert_eq!(resolve_prefer_dark_override(None, false), None);
        assert_eq!(resolve_prefer_dark_override(None, true), None);
    }

    #[test]
    fn explicit_theme_reasserts_only_after_a_conflicting_change() {
        assert_eq!(
            resolve_prefer_dark_override(Some(Theme::Dark), false),
            Some(true)
        );
        assert_eq!(
            resolve_prefer_dark_override(Some(Theme::Light), true),
            Some(false)
        );
        // The notification emitted by reapplying the value must be a no-op.
        assert_eq!(resolve_prefer_dark_override(Some(Theme::Dark), true), None);
        assert_eq!(
            resolve_prefer_dark_override(Some(Theme::Light), false),
            None
        );
    }
}
