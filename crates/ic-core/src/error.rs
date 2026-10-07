//! Errors from starting the engine. Everything that goes wrong later is
//! reported through [`crate::CoreEvent`]s.

/// [`crate::start`] failed.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// The tokio runtime couldn't be built.
    #[error("couldn't build the runtime: {0}")]
    Runtime(#[source] std::io::Error),
    /// The runtime thread couldn't be spawned.
    #[error("couldn't spawn the runtime thread: {0}")]
    Thread(#[source] std::io::Error),
}
