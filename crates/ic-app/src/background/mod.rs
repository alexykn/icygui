//! The app in the background (PLAN.md D4; BG-01..05, REL-07): it keeps
//! running in the tray / menu bar when its window closes and keeps
//! notifying, starts at login without its window, runs once per user, and
//! has the macOS application menus.
//!
//! - [`window`]: the main window closes and comes back; quitting only on
//!   purpose (BG-01);
//! - [`tray`]: the tray icon, tinted with the worst unhandled state, its
//!   tooltip and menu (BG-02, REL-07);
//! - [`autostart`]: launch at login, `--background` (BG-03);
//! - [`instance`]: one instance per user; a second launch brings the
//!   window forward (BG-04);
//! - [`menus`]: the app, Edit, View and Window menus with their shortcuts
//!   (BG-05).

pub(crate) mod autostart;
pub(crate) mod instance;
pub(crate) mod menus;
pub(crate) mod tray;
pub(crate) mod window;
