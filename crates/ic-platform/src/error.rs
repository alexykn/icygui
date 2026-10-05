//! [`PlatformError`]: failures of the tray and launch-at-login adapters.
//!
//! Secret-store failures use the core's own
//! [`SecretError`](ic_core::ports::SecretError), because that is what the
//! [`SecretStore`](ic_core::ports::SecretStore) port returns.

use std::io;
use std::path::PathBuf;

/// What went wrong in an OS adapter.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The tray / menu-bar icon could not be created: no D-Bus session bus
    /// on Linux, or not called on the main thread on macOS.
    #[error("cannot create the tray icon: {0}")]
    Tray(String),

    /// The home directory is unknown (`HOME` is unset and the user database
    /// has no entry).
    #[error("cannot determine the home directory")]
    NoHomeDirectory,

    /// An argument can't be used, for example a relative executable path or
    /// an application id with a path separator.
    #[error("invalid {what}: {reason}")]
    InvalidArgument {
        /// Which argument.
        what: &'static str,
        /// Why it was rejected.
        reason: String,
    },

    /// Reading, writing or removing a file failed.
    #[error("cannot {action} {path}: {source}", path = .path.display())]
    Io {
        /// What was being done ("write", "remove", …).
        action: &'static str,
        /// The file or directory.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// Launch at login isn't implemented on this platform.
    #[error("launch at login is not supported on this platform")]
    Unsupported,
}

impl PlatformError {
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    pub(crate) fn invalid(what: &'static str, reason: impl Into<String>) -> Self {
        Self::InvalidArgument {
            what,
            reason: reason.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_name_the_file_and_the_cause() {
        let error = PlatformError::io(
            "write",
            "/tmp/x.desktop",
            io::Error::from(io::ErrorKind::PermissionDenied),
        );
        let message = error.to_string();
        assert!(
            message.starts_with("cannot write /tmp/x.desktop: "),
            "{message}"
        );
        assert!(std::error::Error::source(&error).is_some());

        let error = PlatformError::invalid("executable path", "must be absolute");
        assert_eq!(
            error.to_string(),
            "invalid executable path: must be absolute"
        );
    }

    #[test]
    fn errors_cross_threads() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<PlatformError>();
    }
}
