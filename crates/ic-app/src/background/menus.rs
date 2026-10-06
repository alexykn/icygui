//! The application menus (BG-05): on macOS the menu bar shows them, with
//! the shortcuts bound to their actions (`cmd` there, `ctrl` elsewhere).
//!
//! - *icygui*: About icygui, Settings… (`⌘,`), Services, Hide icygui
//!   (`⌘H`), Hide Others (`⌥⌘H`), Show All, Quit icygui (`⌘Q`, which
//!   quits even when the app keeps running in the tray);
//! - *Edit*: Undo, Redo, Cut, Copy, Paste, Select All for the text fields
//!   (as the system's own edit actions);
//! - *View*: the sidebar, the command palette, the notification centre;
//! - *Window*: Minimize (`⌘M`), Zoom, and the main window (reopened from
//!   the tray).
//!
//! On Linux GPUI keeps the menus without showing them; the same actions
//! are reached by their keys, the palette and the tray. The handlers here
//! are global, so the menu works with no window open (macOS keeps the
//! menu bar while the app runs in the background): they open the window
//! first.

use gpui::{Action, App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType};
use ic_ui_kit::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};

use super::window;
use crate::actions::{OpenNotifications, OpenSettings, Quit, ShowAbout, ShowWindow};
use crate::settings::SettingsTab;
use crate::workspace::{ToggleCommandPalette, ToggleSidebar};

/// Hides the app (macOS).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Hide;

/// Hides the other apps (macOS).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct HideOthers;

/// Shows every app again (macOS).
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct ShowAll;

/// Minimizes the main window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Minimize;

/// Zooms (maximizes or restores) the main window.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct Zoom;

/// The menus, for `macos` (the system's own items) or not.
pub(crate) fn menus(macos: bool) -> Vec<Menu> {
    let name = crate::APP_NAME;
    let mut app = vec![
        MenuItem::action(format!("About {name}"), ShowAbout),
        MenuItem::separator(),
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
    ];
    if macos {
        app.extend([
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {name}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
        ]);
    }
    app.push(MenuItem::action(format!("Quit {name}"), Quit));
    vec![
        Menu::new(name).items(app),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Command Palette", ToggleCommandPalette),
            MenuItem::action("Notifications", OpenNotifications),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
            MenuItem::separator(),
            MenuItem::action(format!("Show {name}"), ShowWindow),
        ]),
    ]
}

/// The macOS shortcuts of the app and window menus (`⌘H`, `⌥⌘H`, `⌘M`);
/// none elsewhere, where `super-h` and the like belong to the desktop.
pub(crate) fn key_bindings(macos: bool) -> Vec<KeyBinding> {
    if !macos {
        return Vec::new();
    }
    vec![
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]
}

/// Sets the menus, their macOS shortcuts and the global handlers of their
/// actions (they run when no view handled the action, also with no window
/// open).
pub(crate) fn install(cx: &mut App) {
    let macos = cfg!(target_os = "macos");
    cx.set_menus(menus(macos));
    cx.bind_keys(key_bindings(macos));
    cx.on_action(|_: &Quit, cx: &mut App| {
        tracing::info!("quit");
        cx.quit();
    });
    cx.on_action(|_: &ShowWindow, cx: &mut App| {
        window::show(cx);
    });
    cx.on_action(|_: &OpenSettings, cx: &mut App| {
        window::with_workspace(cx, |workspace, window, cx| {
            workspace.open_settings(SettingsTab::General, None, window, cx);
        });
    });
    cx.on_action(|_: &ShowAbout, cx: &mut App| {
        window::with_workspace(cx, |workspace, window, cx| {
            workspace.open_about(window, cx);
        });
    });
    cx.on_action(|_: &OpenNotifications, cx: &mut App| {
        window::with_workspace(cx, |workspace, window, cx| {
            workspace.open_notifications(window, cx);
        });
    });
    cx.on_action(|_: &ToggleCommandPalette, cx: &mut App| {
        window::with_workspace(cx, |workspace, window, cx| {
            workspace.open_palette(window, cx);
        });
    });
    cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
    cx.on_action(|_: &HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx: &mut App| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx: &mut App| {
        if let Some(window) = window::main_window(cx) {
            let _ = window.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx: &mut App| {
        if let Some(window) = window::main_window(cx) {
            let _ = window.update(cx, |_, window, _| window.zoom_window());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(menu: &Menu) -> Vec<String> {
        menu.items
            .iter()
            .map(|item| match item {
                MenuItem::Separator => "—".to_owned(),
                MenuItem::Submenu(menu) => format!("{} ▸", menu.name),
                MenuItem::SystemMenu(menu) => format!("{} ▸", menu.name),
                MenuItem::Action { name, .. } => name.to_string(),
            })
            .collect()
    }

    #[test]
    fn the_app_menu_has_about_settings_and_quit() {
        let mac = menus(true);
        let titles: Vec<&str> = mac.iter().map(|menu| menu.name.as_ref()).collect();
        assert_eq!(titles, ["icygui", "Edit", "View", "Window"]);
        assert_eq!(
            names(&mac[0]),
            [
                "About icygui",
                "—",
                "Settings…",
                "—",
                "Services ▸",
                "—",
                "Hide icygui",
                "Hide Others",
                "Show All",
                "—",
                "Quit icygui"
            ]
        );
        assert_eq!(
            names(&mac[1]),
            ["Undo", "Redo", "—", "Cut", "Copy", "Paste", "Select All"]
        );
        // Elsewhere without the system's own items.
        assert_eq!(
            names(&menus(false)[0]),
            ["About icygui", "—", "Settings…", "—", "Quit icygui"]
        );
    }

    #[test]
    fn hide_and_minimize_have_their_macos_shortcuts_only_there() {
        // (key, ⌥, ⌘, action); `unparse` would name ⌘ by the test's
        // platform.
        let mac: Vec<(String, bool, bool, &str)> = key_bindings(true)
            .iter()
            .map(|binding| {
                let keystroke = &binding.keystrokes()[0];
                (
                    keystroke.key().to_owned(),
                    keystroke.modifiers().alt,
                    keystroke.modifiers().platform,
                    binding.action().name(),
                )
            })
            .collect();
        assert_eq!(
            mac,
            [
                ("h".to_owned(), false, true, "icygui::Hide"),
                ("h".to_owned(), true, true, "icygui::HideOthers"),
                ("m".to_owned(), false, true, "icygui::Minimize"),
            ]
        );
        assert!(key_bindings(false).is_empty());
    }
}
