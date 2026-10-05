//! Ports: what the core needs from the operating system. Implemented in
//! `ic-platform` (secrets) and `ic-app` (notifications, via GPUI); tests use
//! in-memory fakes.

use ic_model::Timestamp;
use ic_rules::{LocalTime, NotificationIntent};
use secrecy::SecretString;

/// Error from the platform's secret store.
#[derive(Debug, thiserror::Error)]
#[error("secret store: {message}")]
pub struct SecretError {
    /// What went wrong, for logs and the UI.
    pub message: String,
}

impl SecretError {
    /// An error with a message.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Stores credentials outside the config file (Keychain, Secret Service).
/// `account` is the environment id.
pub trait SecretStore: Send + Sync + 'static {
    /// Reads a secret; `Ok(None)` if there is none.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused access.
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError>;

    /// Creates or replaces a secret.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused access.
    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretError>;

    /// Deletes a secret; succeeds if there was none.
    ///
    /// # Errors
    ///
    /// The store is unavailable or refused access.
    fn delete(&self, account: &str) -> Result<(), SecretError>;
}

/// Shows OS notifications. Called from the core's runtime thread; the
/// implementation hands the intent to the UI thread.
pub trait Notifier: Send + Sync + 'static {
    /// Shows `intent` (callers never pass silent intents).
    fn notify(&self, intent: &NotificationIntent);
}

/// Wall-clock time, injectable for tests.
pub trait Clock: Send + Sync + 'static {
    /// Now.
    fn now(&self) -> Timestamp;
    /// Local weekday and time of day, for quiet hours.
    fn local(&self) -> LocalTime;
}
