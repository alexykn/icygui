//! Errors of the mock server and its control handle.

/// Everything that can go wrong when starting or driving a mock server.
#[derive(Debug, thiserror::Error)]
pub enum MockError {
    /// Binding the listener or another I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Generating or loading certificates failed.
    #[error("certificate error: {0}")]
    Certificate(String),
    /// Building the TLS configuration failed.
    #[error("TLS error: {0}")]
    Tls(String),
    /// The configuration or scenario is inconsistent.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// A control call referenced an object that doesn't exist.
    #[error("unknown object: {0}")]
    UnknownObject(String),
    /// A control call was rejected the same way Icinga would reject it.
    #[error("rejected: {0}")]
    Rejected(String),
    /// The server is shut down.
    #[error("the mock server is shut down")]
    ShutDown,
}
