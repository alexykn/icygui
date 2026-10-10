//! OS adapters for macOS and Linux (PLAN.md §3.1 principle 5):
//!
//! - [`KeyringSecrets`]: environment passwords in the macOS login keychain
//!   or the freedesktop Secret Service, behind the core's
//!   [`SecretStore`](ic_core::ports::SecretStore) port, and
//!   [`DirSecrets`], plain password files for development and headless
//!   runs only (when [`DEV_SECRETS_ENV`] is set, never by default);
//! - [`tray`]: the tray / menu-bar icon tinted with the worst state, with
//!   its menu (open, pause notifications, environments, quit);
//! - [`autostart`]: launch at login, as a launch agent on macOS or an XDG
//!   autostart entry on Linux.
//!
//! Native notifications go through GPUI and live in `ic-app`.

pub mod autostart;
mod dev_secrets;
mod error;
mod secrets;
pub mod tray;

pub use dev_secrets::{DEV_SECRETS_ENV, DirSecrets};
pub use error::PlatformError;
pub use secrets::{KeyringSecrets, SERVICE};
