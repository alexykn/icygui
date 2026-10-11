//! OS adapters for macOS and Linux (PLAN.md §3.1 principle 5):
//!
//! - [`KeyringSecrets`]: environment passwords in the macOS login keychain
//!   or the freedesktop Secret Service, behind the core's
//!   [`SecretStore`](ic_core::ports::SecretStore) port: the only place icygui
//!   keeps credentials (headless runs, such as the demo harness, start a
//!   Secret Service of their own; `examples/store_secret.rs`);
//! - [`tray`]: the tray / menu-bar icon tinted with the worst state, with
//!   its menu (open, pause notifications, environments, quit);
//! - [`autostart`]: launch at login, as a launch agent on macOS or an XDG
//!   autostart entry on Linux.
//!
//! Native notifications go through GPUI and live in `ic-app`.

pub mod autostart;
mod error;
mod secrets;
pub mod tray;

pub use error::PlatformError;
pub use secrets::{KeyringSecrets, SERVICE};
