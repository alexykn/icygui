//! Appearance (UI-02, mock-up 03) on the fixture: the theme follows the
//! setting at once, and *follow system* follows the desktop; the interface
//! size scales text and spacing (the rows the pointer hits move); compact
//! rows put about twice the rows on screen; the times in lists read as
//! clock times. Every change comes through the settings panel's controls
//! or the settings (as an edited settings file does).
//!
//! Positions are the 1440 × 900 window's: the panel's appearance page draws
//! its controls where the settings tests say, the list's rows start under
//! the 41px header and the 37px summary bar.

use gpui::{App, Entity, Modifiers, WindowAppearance, point, px};
use ic_config::{Appearance, InterfaceSize, ListTimes, RowDensity, ThemeChoice};
use ic_model::Timestamp;
use ic_ui_kit::{ActiveTheme as _, Colors, Density, ThemeMode};

use super::{Harness, run};
use crate::appearance;
use crate::fixture::FixtureOptions;
use crate::settings::{SettingsPage, SettingsPanel};

/// The appearance page's theme control: *follow system*, *dark*, *light*.
const THEME_Y: f32 = 195.;
const FOLLOW_SYSTEM_X: f32 = 1042.;
const LIGHT_X: f32 = 1194.;
/// Its interface size control's *large*.
const LARGE: (f32, f32) = (1194., 257.);
/// Its row density control's *compact*.
const COMPACT: (f32, f32) = (1186., 364.);
/// Its times control's *clock*.
const CLOCK: (f32, f32) = (1193., 426.);

/// Opens the settings panel on its appearance page.
fn appearance_page(app: &Harness, cx: &mut App) -> Entity<SettingsPanel> {
    app.keys(cx, "ctrl-,");
    let panel = app.workspace.read(cx).settings().unwrap().clone();
    app.in_window(cx, |window, cx| {
        panel.update(cx, |panel, cx| {
            panel.show_page(SettingsPage::Appearance, window, cx);
        });
    });
    app.draw(cx);
    panel
}

fn click(app: &Harness, cx: &mut App, (x, y): (f32, f32)) {
    app.click(cx, point(px(x), px(y)), Modifiers::default());
}

/// Changes the appearance settings as an edited settings file does.
fn set(app: &Harness, cx: &mut App, change: impl FnOnce(&mut Appearance)) {
    app.state.update(cx, |state, cx| {
        let mut appearance = *state.appearance();
        change(&mut appearance);
        if state.set_appearance(appearance) {
            cx.notify();
        }
    });
    app.draw(cx);
}

fn settings(app: &Harness, cx: &App) -> Appearance {
    *app.state.read(cx).appearance()
}

#[test]
fn the_theme_follows_the_setting_and_the_desktop() {
    run(FixtureOptions::default(), |app, cx| {
        // Follow system by default; the headless window reports a dark
        // desktop.
        assert_eq!(settings(app, cx).theme, ThemeChoice::System);
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
        assert_eq!(cx.theme().colors, Colors::dark());

        // Light from the panel: at once.
        appearance_page(app, cx);
        click(app, cx, (LIGHT_X, THEME_Y));
        assert_eq!(settings(app, cx).theme, ThemeChoice::Light);
        assert_eq!(cx.theme().mode, ThemeMode::Light);
        assert_eq!(cx.theme().colors, Colors::light());

        // A forced theme ignores the desktop.
        appearance::system_changed(WindowAppearance::Dark, settings(app, cx), cx);
        app.draw(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Light);

        // Follow system takes the desktop's mode, and follows it live.
        click(app, cx, (FOLLOW_SYSTEM_X, THEME_Y));
        assert_eq!(settings(app, cx).theme, ThemeChoice::System);
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
        appearance::system_changed(WindowAppearance::Light, settings(app, cx), cx);
        app.draw(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Light);
        appearance::system_changed(WindowAppearance::VibrantDark, settings(app, cx), cx);
        app.draw(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Dark);

        // A settings file that says dark: dark, whatever the desktop.
        set(app, cx, |appearance| appearance.theme = ThemeChoice::Dark);
        appearance::system_changed(WindowAppearance::Light, settings(app, cx), cx);
        app.draw(cx);
        assert_eq!(cx.theme().mode, ThemeMode::Dark);
    });
}

#[test]
fn the_interface_size_scales_text_and_spacing_live() {
    run(FixtureOptions::default(), |app, cx| {
        let base = cx.theme().text.row;
        // At 100 % y = 215 is the third row (78 + 2 × 61 = 200 to 261).
        app.click(cx, point(px(700.), px(215.)), Modifiers::default());
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(2));

        appearance_page(app, cx);
        click(app, cx, LARGE);
        assert_eq!(settings(app, cx).interface_size, InterfaceSize::Large);
        let theme = cx.theme().clone();
        assert!((theme.scale - 1.15).abs() < f32::EPSILON);
        assert!((ic_ui_kit::scale() - 1.15).abs() < f32::EPSILON);
        assert_eq!(theme.text.row, base * 1.15);
        assert_eq!(theme.metrics.sidebar_width, px(345.));
        assert_eq!(ic_ui_kit::px(10.), px(11.5), "lengths in views follow");
        app.keys(cx, "escape");
        assert!(app.workspace.read(cx).settings().is_none());

        // The rows grew: the header 47, the summary bar about 42 and the
        // rows 70 tall, so the same point is in the second row now.
        app.click(cx, point(px(700.), px(215.)), Modifiers::default());
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));

        // 90 %: the header 37, the summary bar about 33, rows 55: y = 250
        // is the fourth row (at 100 % it is the third).
        set(app, cx, |appearance| {
            appearance.interface_size = InterfaceSize::Small;
        });
        assert!((cx.theme().scale - 0.9).abs() < f32::EPSILON);
        app.click(cx, point(px(700.), px(250.)), Modifiers::default());
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(3));

        // Back to 100 %: exactly the design's sizes again.
        set(app, cx, |appearance| {
            appearance.interface_size = InterfaceSize::Default;
        });
        assert_eq!(cx.theme().text.row, base);
        assert_eq!(ic_ui_kit::px(10.), px(10.));
        app.click(cx, point(px(700.), px(250.)), Modifiers::default());
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(2));
    });
}

#[test]
fn compact_rows_put_about_twice_the_rows_on_screen() {
    // A list longer than any screen (the load-test dashboard).
    let options = FixtureOptions {
        generated_rows: 200,
    };
    run(options, |app, cx| {
        let comfortable = app.dashboard(cx).read(cx).visible_rows().len();
        assert!(comfortable > 5, "{comfortable}");

        appearance_page(app, cx);
        click(app, cx, COMPACT);
        assert_eq!(settings(app, cx).row_density, RowDensity::Compact);
        assert_eq!(cx.theme().density, Density::Compact);
        assert_eq!(cx.theme().metrics.row_height, px(32.));
        app.keys(cx, "escape");

        let compact = app.dashboard(cx).read(cx).visible_rows().len();
        assert!(
            compact * 10 >= comfortable * 17,
            "{compact} rows instead of {comfortable}"
        );
        // Rows are 33 tall with their rule: y = 78 + 3 × 33 + 16 is the
        // fourth row (at comfortable density it is the second).
        app.click(cx, point(px(700.), px(193.)), Modifiers::default());
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(3));
        // The keys still page through the virtualised list.
        app.keys(cx, "pagedown");
        let (index, _) = app.cursor(cx).unwrap();
        assert!(index > 3 + comfortable, "a page is a compact page: {index}");

        // Back to comfortable (scrolled by a page now: one row more may
        // peek in).
        set(app, cx, |appearance| {
            appearance.row_density = RowDensity::Comfortable;
        });
        let back = app.dashboard(cx).read(cx).visible_rows().len();
        assert!(
            back.abs_diff(comfortable) <= 1,
            "{back} rows, {comfortable} before"
        );
    });
}

#[test]
fn times_in_lists_read_as_clock_times() {
    run(FixtureOptions::default(), |app, cx| {
        let relative = app.dashboard(cx).read(cx).shown_times().to_vec();
        assert!(!relative.is_empty());
        // `14m`, `2h`, `41d`: a number and a unit.
        assert!(
            relative
                .iter()
                .all(|time| time.ends_with(['s', 'm', 'h', 'd', 'y'])),
            "{relative:?}"
        );

        appearance_page(app, cx);
        click(app, cx, CLOCK);
        assert_eq!(settings(app, cx).list_times, ListTimes::Clock);
        app.keys(cx, "escape");
        let clock = app.dashboard(cx).read(cx).shown_times().to_vec();
        assert_eq!(clock.len(), relative.len());
        // The same rows, since when: what the list's clock format says for
        // each row's state change.
        let now = Timestamp::now();
        let expected: Vec<String> = {
            let state = app.state.read(cx);
            let snapshot = state.snapshot();
            let visible = app.dashboard(cx).read(cx).visible_rows();
            app.rows(cx)[visible]
                .iter()
                .filter_map(|row| match row {
                    ic_core::snapshot::DashboardRow::Object(key) => {
                        crate::dashboard::rows::object_row(snapshot, key, now)
                    }
                    ic_core::snapshot::DashboardRow::Group { .. } => None,
                })
                .map(|row| row.clock)
                .collect()
        };
        assert_eq!(clock, expected);
        assert!(
            clock
                .iter()
                .all(|time| time.contains(':') || time.split(' ').count() == 2 || time.len() == 4),
            "13:58, Oct 3 or 2025: {clock:?}"
        );

        // Compact rows show the same times at the right end.
        set(app, cx, |appearance| {
            appearance.row_density = RowDensity::Compact;
        });
        let compact = app.dashboard(cx).read(cx).shown_times().to_vec();
        assert_eq!(&compact[..clock.len()], clock.as_slice());
    });
}
