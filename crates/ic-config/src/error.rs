//! The crate's error type.

use std::io;
use std::path::PathBuf;

use crate::validate::ValidationIssue;

/// Everything that can go wrong finding, reading, writing or interpreting
/// settings.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The operating system reports no home directory, so the standard
    /// locations for settings, data and logs are unknown.
    #[error("cannot locate the settings directory: the system reports no home directory")]
    NoHomeDirectory,

    /// A file system operation failed.
    #[error("{action} {}: {source}", path.display())]
    Io {
        /// What was being done, such as `reading` or `replacing`.
        action: &'static str,
        /// The file or directory involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The text is not valid TOML, or doesn't have the expected structure
    /// (wrong types, unknown enum values, missing required keys). The
    /// message says where, with a line and column when they are known.
    #[error("{message}")]
    Parse {
        /// What is wrong and where.
        message: String,
    },

    /// The `version` key is present but not a whole number.
    #[error("invalid format version: {0}")]
    InvalidVersion(String),

    /// The text was written by a newer version of icygui.
    #[error(
        "format version {found} is newer than this version of icygui supports ({supported}); \
         update icygui to use these settings"
    )]
    UnsupportedVersion {
        /// The version in the text.
        found: u64,
        /// The newest version this build understands.
        supported: u64,
    },

    /// The settings can't be written as TOML, for example because a path
    /// is not valid UTF-8.
    #[error("cannot write settings as TOML: {0}")]
    Serialize(String),

    /// A certificate fingerprint is not 32 bytes of hex.
    #[error("invalid SHA-256 fingerprint: {0}")]
    InvalidFingerprint(String),

    /// An environment's API URL can't be used.
    #[error("invalid API URL `{url}`: {reason}")]
    InvalidUrl {
        /// The URL as configured.
        url: String,
        /// Why it can't be used.
        reason: String,
    },

    /// Text given to [`import_groups`](crate::import_groups) is not a
    /// dashboard export.
    #[error("not an icygui dashboard export: {0}")]
    NotAnExport(String),

    /// The content parsed but breaks the rules [`Config::validate`]
    /// checks (used for imports).
    ///
    /// [`Config::validate`]: crate::Config::validate
    #[error("invalid content: {}", join_issues(.0))]
    Invalid(Vec<ValidationIssue>),
}

impl ConfigError {
    /// A [`ConfigError::Io`] for `action` on `path`.
    pub(crate) fn io(action: &'static str, path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io {
            action,
            path: path.into(),
            source,
        }
    }

    /// A [`ConfigError::Parse`] from a TOML error, without the trailing
    /// newline the TOML crate adds to its messages.
    pub(crate) fn parse(error: &toml::de::Error) -> Self {
        Self::Parse {
            message: error.to_string().trim_end().to_owned(),
        }
    }
}

fn join_issues(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}
