//! Starting the engine on its own thread, and the handle the UI keeps.

use std::fmt;
use std::sync::mpsc as std_mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

use futures::channel::mpsc as futures_mpsc;
use tokio::sync::{mpsc, oneshot};

use crate::command::{Command, CoreEvent};
use crate::engine::Engine;
use crate::error::CoreError;
use crate::spec::{EnvironmentSpec, Ports, Tuning};

/// How long the runtime may take to wind down its tasks after the engine
/// stopped (blocking secret-store reads can't be cancelled).
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(1);

/// Starts the engine for `spec` on a dedicated thread with its own tokio
/// runtime: it connects right away and keeps the environment in sync until
/// [`CoreHandle::shutdown`].
///
/// # Errors
///
/// The runtime or its thread couldn't be created.
pub fn start(spec: EnvironmentSpec, ports: Ports) -> Result<CoreHandle, CoreError> {
    start_with_tuning(spec, ports, Tuning::default())
}

/// [`start`] with other timing (tests shorten backoff, polls and
/// throttles instead of sleeping).
///
/// # Errors
///
/// The runtime or its thread couldn't be created.
pub fn start_with_tuning(
    spec: EnvironmentSpec,
    ports: Ports,
    tuning: Tuning,
) -> Result<CoreHandle, CoreError> {
    let (commands_tx, commands_rx) = mpsc::unbounded_channel();
    let (events_tx, events_rx) = futures_mpsc::unbounded();
    let (internal_tx, internal_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let (ready_tx, ready_rx) = std_mpsc::sync_channel(1);
    let (done_tx, done_rx) = std_mpsc::channel();
    let shutdown_timeout = tuning.shutdown_timeout;
    let environment = spec.environment.name.clone();
    let engine = Engine::new(spec, ports, tuning, events_tx, internal_tx);
    // The runtime is built, run and dropped on its own thread: a runtime
    // must not be dropped where the caller might be async.
    let thread = std::thread::Builder::new()
        .name("icygui-core".to_owned())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("icygui-core-worker")
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = ready_tx.send(Err(error));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            runtime.block_on(engine.run(commands_rx, internal_rx, shutdown_rx));
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
            let _ = done_tx.send(());
        })
        .map_err(CoreError::Thread)?;
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = thread.join();
            return Err(CoreError::Runtime(error));
        }
        Err(_) => {
            let _ = thread.join();
            return Err(CoreError::Runtime(std::io::Error::other(
                "the runtime thread ended before starting",
            )));
        }
    }
    tracing::info!(%environment, "engine started");
    Ok(CoreHandle {
        commands: commands_tx,
        events: Some(events_rx),
        shutdown: Some(shutdown_tx),
        thread: Some(thread),
        done: done_rx,
        shutdown_timeout,
    })
}

/// The UI's handle on a running engine. Dropping it stops the engine
/// without waiting; [`CoreHandle::shutdown`] waits (bounded).
pub struct CoreHandle {
    commands: mpsc::UnboundedSender<Command>,
    events: Option<futures_mpsc::UnboundedReceiver<CoreEvent>>,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
    done: std_mpsc::Receiver<()>,
    shutdown_timeout: Duration,
}

impl fmt::Debug for CoreHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CoreHandle")
            .field("running", &self.thread.is_some())
            .finish_non_exhaustive()
    }
}

impl CoreHandle {
    /// Sends a command. Never blocks; a command sent after the engine
    /// stopped is dropped (and logged).
    pub fn send(&self, command: Command) {
        if let Err(error) = self.commands.send(command) {
            tracing::warn!(command = ?error.0, "the engine has stopped; command dropped");
        }
    }

    /// The engine's events, in order. There is one receiver; the first
    /// call takes it (the UI bridge), later calls get `None`.
    pub fn take_events(&mut self) -> Option<futures_mpsc::UnboundedReceiver<CoreEvent>> {
        self.events.take()
    }

    /// Stops the engine: closes the event stream and every request, then
    /// joins the runtime thread, waiting at most the tuning's
    /// `shutdown_timeout` (5 s); a thread that takes longer is left to
    /// finish on its own.
    pub fn shutdown(mut self) {
        self.signal();
        let Some(thread) = self.thread.take() else {
            return;
        };
        match self.done.recv_timeout(self.shutdown_timeout) {
            Ok(()) | Err(std_mpsc::RecvTimeoutError::Disconnected) => {
                if thread.join().is_err() {
                    tracing::error!("the engine thread panicked");
                }
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {
                tracing::warn!(
                    timeout = ?self.shutdown_timeout,
                    "the engine didn't stop in time; leaving it behind"
                );
            }
        }
    }

    fn signal(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            // Already stopped if the engine is gone.
            let _ = shutdown.send(());
        }
    }
}

impl Drop for CoreHandle {
    fn drop(&mut self) {
        self.signal();
    }
}
