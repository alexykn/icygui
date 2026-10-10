//! The appearance settings in effect (UI-02): the theme (following the
//! desktop's light or dark mode, or forced), the interface size and the row
//! density, applied to every window as soon as they change.
//!
//! The settings live in [`AppState::appearance`]; the main window applies
//! them whenever the state changes and whenever the desktop switches between
//! light and dark ([`window_opened`], [`system_changed`]). Applying installs a
//! new [`Theme`] only when the result differs from the one in effect, so the
//! frequent state notifications cost a comparison. The times in lists are
//! not part of the theme: the lists read them from the settings.
//!
//! [`AppState::appearance`]: crate::app_state::AppState::appearance

use gpui::{App, Global, WindowAppearance};
use ic_config::{Appearance, RowDensity, ThemeChoice};
use ic_ui_kit::{ActiveTheme as _, Density, Theme, ThemeMode};

/// The desktop's light or dark mode, as the main window last reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DesktopMode(ThemeMode);

impl Global for DesktopMode {}

/// The theme mode for a window appearance: the vibrant variants are macOS's
/// translucent ones of the same mode.
pub(crate) fn mode_of(appearance: WindowAppearance) -> ThemeMode {
    match appearance {
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeMode::Light,
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
    }
}

/// The theme `appearance` asks for while the desktop is in `desktop` mode.
pub(crate) fn theme_for(appearance: Appearance, desktop: ThemeMode) -> Theme {
    let mode = match appearance.theme {
        ThemeChoice::System => desktop,
        ThemeChoice::Dark => ThemeMode::Dark,
        ThemeChoice::Light => ThemeMode::Light,
    };
    let density = match appearance.row_density {
        RowDensity::Comfortable => Density::Comfortable,
        RowDensity::Compact => Density::Compact,
    };
    Theme::new(mode, appearance.interface_size.scale(), density)
}

/// The desktop's mode: as the main window last reported it, or as the
/// platform says before a window did.
fn desktop_mode(cx: &App) -> ThemeMode {
    cx.try_global::<DesktopMode>()
        .map_or_else(|| mode_of(cx.window_appearance()), |mode| mode.0)
}

/// Puts `appearance` into effect, unless it already is: installs the theme
/// it asks for and redraws every window.
pub(crate) fn apply(appearance: Appearance, cx: &mut App) {
    let theme = theme_for(appearance, desktop_mode(cx));
    if !cx.has_global::<Theme>() || differs(cx.theme(), &theme) {
        ic_ui_kit::set_theme(theme, cx);
    }
}

/// A window opened: it reports the desktop's mode (`appearance`), and
/// `settings` go into effect before its first frame.
pub(crate) fn window_opened(appearance: WindowAppearance, settings: Appearance, cx: &mut App) {
    system_changed(appearance, settings, cx);
}

/// The desktop switched between light and dark (the main window reports
/// `appearance`): *follow system* follows at once.
pub(crate) fn system_changed(appearance: WindowAppearance, settings: Appearance, cx: &mut App) {
    cx.set_global(DesktopMode(mode_of(appearance)));
    apply(settings, cx);
}

/// Whether `next` looks different from `current`.
fn differs(current: &Theme, next: &Theme) -> bool {
    current.mode != next.mode
        || current.density != next.density
        || (current.scale - next.scale).abs() > f32::EPSILON
}

#[cfg(test)]
mod tests {
    use ic_config::{InterfaceSize, ListTimes};

    use super::*;

    fn appearance(theme: ThemeChoice) -> Appearance {
        Appearance {
            theme,
            ..Appearance::default()
        }
    }

    #[test]
    fn follow_system_takes_the_desktops_mode() {
        let system = appearance(ThemeChoice::System);
        assert_eq!(theme_for(system, ThemeMode::Light).mode, ThemeMode::Light);
        assert_eq!(theme_for(system, ThemeMode::Dark).mode, ThemeMode::Dark);
        // Forced themes ignore it.
        for desktop in [ThemeMode::Light, ThemeMode::Dark] {
            assert_eq!(
                theme_for(appearance(ThemeChoice::Dark), desktop).mode,
                ThemeMode::Dark
            );
            assert_eq!(
                theme_for(appearance(ThemeChoice::Light), desktop).mode,
                ThemeMode::Light
            );
        }
    }

    #[test]
    fn vibrant_appearances_are_their_modes() {
        assert_eq!(mode_of(WindowAppearance::VibrantDark), ThemeMode::Dark);
        assert_eq!(mode_of(WindowAppearance::VibrantLight), ThemeMode::Light);
        assert_eq!(mode_of(WindowAppearance::Dark), ThemeMode::Dark);
        assert_eq!(mode_of(WindowAppearance::Light), ThemeMode::Light);
    }

    #[test]
    fn size_and_density_build_into_the_theme() {
        let settings = Appearance {
            theme: ThemeChoice::Dark,
            interface_size: InterfaceSize::Large,
            row_density: RowDensity::Compact,
            list_times: ListTimes::Clock,
            hide_handled: ic_config::HideHandled::ALL,
        };
        let theme = theme_for(settings, ThemeMode::Light);
        assert!((theme.scale - 1.15).abs() < f32::EPSILON);
        assert_eq!(theme.density, Density::Compact);
        assert_eq!(theme.colors, Theme::dark().colors);
        // The times in lists change no theme.
        let relative = Appearance {
            list_times: ListTimes::Relative,
            ..settings
        };
        assert!(!differs(&theme, &theme_for(relative, ThemeMode::Light)));
        assert!(differs(&theme, &Theme::dark()));
    }
}
