//! One instance per user (BG-04): a second launch hands over to the
//! running one, which brings its window forward (or opens it from the
//! tray), and exits.
//!
//! The running instance holds an exclusive lock on `instance.lock` and
//! listens on a Unix socket next to it (`instance.sock`), in
//! `$XDG_RUNTIME_DIR/io.github.alexykn.icygui` on Linux (a private tmpfs),
//! else the data directory; both are private to the user (0700). A second
//! launch finds the lock taken, connects, says `show` (or `background`
//! when started at login: then nothing changes) and waits for `ok`. A lock
//! left by a crashed instance is released by the system with its process,
//! and a stale socket is replaced.
//!
//! The demo doesn't take part: it runs beside the real app.

use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

/// What a second launch asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Request {
    /// Bring the window forward (open it if it's closed).
    Show,
    /// Started at login while running: nothing to do.
    Background,
}

impl Request {
    fn word(self) -> &'static str {
        match self {
            Self::Show => "show",
            Self::Background => "background",
        }
    }

    fn parse(word: &str) -> Option<Self> {
        match word.trim() {
            "show" => Some(Self::Show),
            "background" => Some(Self::Background),
            _ => None,
        }
    }
}

/// The outcome of starting.
#[derive(Debug)]
pub(crate) enum Claim {
    /// This is the instance: requests from later launches arrive here.
    Primary(Instance),
    /// Another instance runs and was told.
    Forwarded,
}

/// The running instance's lock and listener.
#[derive(Debug)]
pub(crate) struct Instance {
    /// Held for the process's life.
    _lock: Option<File>,
    socket: Option<PathBuf>,
    requests: Option<UnboundedReceiver<Request>>,
}

impl Instance {
    /// The requests of later launches; take once.
    pub(crate) fn requests(&mut self) -> Option<UnboundedReceiver<Request>> {
        self.requests.take()
    }

    /// An instance without a lock (it couldn't be taken here).
    fn unlocked() -> Self {
        Self {
            _lock: None,
            socket: None,
            requests: None,
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if let Some(socket) = &self.socket {
            let _ = fs::remove_file(socket);
        }
    }
}

/// How long a second launch waits for the running one to answer (it may
/// be starting and not listen yet).
const ANSWER_WITHIN: Duration = Duration::from_secs(3);
/// Unix socket paths are limited (104 bytes on macOS, 108 on Linux).
const MAX_SOCKET_PATH: usize = 100;

/// The directory for the lock and the socket.
pub(crate) fn directory(data_dir: &Path) -> PathBuf {
    if cfg!(target_os = "linux")
        && let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && runtime.is_absolute()
    {
        return runtime.join(crate::APP_ID);
    }
    data_dir.to_path_buf()
}

/// Becomes the instance, or hands `request` over to the running one.
///
/// # Errors
///
/// Another instance holds the lock but doesn't answer.
pub(crate) fn claim(dir: &Path, request: Request) -> Result<Claim, String> {
    if let Err(error) = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
    {
        tracing::warn!(%error, dir = %dir.display(), "no directory for the instance lock; not checking for another instance");
        return Ok(Claim::Primary(Instance::unlocked()));
    }
    let lock_path = dir.join("instance.lock");
    let socket = dir.join("instance.sock");
    let lock = match OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(&lock_path)
    {
        Ok(lock) => lock,
        Err(error) => {
            tracing::warn!(%error, path = %lock_path.display(), "the instance lock can't be opened; not checking for another instance");
            return Ok(Claim::Primary(Instance::unlocked()));
        }
    };
    match lock.try_lock() {
        Ok(()) => Ok(Claim::Primary(listen(lock, socket))),
        Err(TryLockError::WouldBlock) => {
            forward(&socket, request)?;
            Ok(Claim::Forwarded)
        }
        Err(TryLockError::Error(error)) => {
            tracing::warn!(%error, "the instance lock isn't supported here; not checking for another instance");
            Ok(Claim::Primary(Instance::unlocked()))
        }
    }
}

/// Holds the lock and listens for later launches.
fn listen(lock: File, socket: PathBuf) -> Instance {
    let mut instance = Instance {
        _lock: Some(lock),
        socket: None,
        requests: None,
    };
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        tracing::warn!(path = %socket.display(), "the instance socket's path is too long; later launches can't hand over");
        return instance;
    }
    // Ours alone holds the lock: a socket file there is stale.
    let _ = fs::remove_file(&socket);
    let listener = match UnixListener::bind(&socket) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, path = %socket.display(), "later launches can't hand over");
            return instance;
        }
    };
    let (sender, receiver) = unbounded();
    let spawned = std::thread::Builder::new()
        .name("icygui-instance".to_owned())
        .spawn(move || accept(&listener, &sender));
    if let Err(error) = spawned {
        tracing::warn!(%error, "later launches can't hand over");
        let _ = fs::remove_file(&socket);
        return instance;
    }
    instance.socket = Some(socket);
    instance.requests = Some(receiver);
    instance
}

/// Answers later launches until the app goes.
fn accept(listener: &UnixListener, requests: &UnboundedSender<Request>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        match answer(&stream) {
            Ok(Some(request)) => {
                tracing::info!(?request, "another launch handed over");
                if requests.unbounded_send(request).is_err() {
                    return;
                }
            }
            Ok(None) => tracing::debug!("an unknown request on the instance socket"),
            Err(error) => tracing::debug!(%error, "a broken request on the instance socket"),
        }
    }
}

/// Reads one request and says `ok`.
fn answer(stream: &UnixStream) -> io::Result<Option<Request>> {
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut line = String::new();
    BufReader::new(stream.try_clone()?)
        .take(64)
        .read_line(&mut line)?;
    let request = Request::parse(&line);
    if request.is_some() {
        let mut stream = stream;
        stream.write_all(b"ok\n")?;
    }
    Ok(request)
}

/// Tells the running instance, waiting for it to listen.
fn forward(socket: &Path, request: Request) -> Result<(), String> {
    let deadline = Instant::now() + ANSWER_WITHIN;
    loop {
        match send(socket, request) {
            Ok(()) => return Ok(()),
            Err(error) if Instant::now() >= deadline => {
                return Err(format!(
                    "icygui is already running but doesn't answer ({error}); quit it, or end \
                     its process, and start again"
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn send(socket: &Path, request: Request) -> io::Result<()> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(format!("{}\n", request.word()).as_bytes())?;
    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply)?;
    if reply.trim() == "ok" {
        Ok(())
    } else {
        Err(io::Error::other(format!("unexpected answer {reply:?}")))
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt as _;

    use futures::StreamExt as _;
    use futures::executor::block_on;

    use super::*;

    #[test]
    fn a_second_launch_hands_over_to_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let instance_dir = dir.path().join("run");
        let Claim::Primary(mut first) = claim(&instance_dir, Request::Show).unwrap() else {
            panic!("the first launch is the instance");
        };
        let mode = fs::metadata(&instance_dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "private directory");
        let mut requests = first.requests().unwrap();
        assert!(first.requests().is_none(), "taken once");

        // A second and a third launch (one at login) hand over and exit.
        assert!(matches!(
            claim(&instance_dir, Request::Show).unwrap(),
            Claim::Forwarded
        ));
        assert!(matches!(
            claim(&instance_dir, Request::Background).unwrap(),
            Claim::Forwarded
        ));
        assert_eq!(block_on(requests.next()), Some(Request::Show));
        assert_eq!(block_on(requests.next()), Some(Request::Background));

        // Once the first quits, the next launch is the instance again
        // (the socket file it left behind is replaced).
        drop(first);
        drop(requests);
        assert!(matches!(
            claim(&instance_dir, Request::Show).unwrap(),
            Claim::Primary(_)
        ));
    }

    #[test]
    fn a_stale_socket_is_replaced_and_garbage_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("instance.sock"), b"left behind").unwrap();
        let Claim::Primary(mut instance) = claim(dir.path(), Request::Show).unwrap() else {
            panic!("no other instance");
        };
        let mut requests = instance.requests().unwrap();
        let mut stream = UnixStream::connect(dir.path().join("instance.sock")).unwrap();
        stream.write_all(b"format c:\n").unwrap();
        let mut reply = String::new();
        let _ = BufReader::new(stream).read_line(&mut reply);
        assert_eq!(reply, "", "no answer to garbage");
        send(&dir.path().join("instance.sock"), Request::Show).unwrap();
        assert_eq!(block_on(requests.next()), Some(Request::Show));
    }

    #[test]
    fn a_lock_held_without_an_answer_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path()).unwrap();
        let held = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.path().join("instance.lock"))
            .unwrap();
        held.lock().unwrap();
        let started = Instant::now();
        let error = claim(dir.path(), Request::Show).unwrap_err();
        assert!(error.contains("already running"), "{error}");
        assert!(started.elapsed() >= ANSWER_WITHIN);
    }

    #[test]
    fn the_runtime_directory_is_preferred_on_linux() {
        let data = Path::new("/home/u/.local/share/icygui");
        let dir = directory(data);
        if cfg!(target_os = "linux") && std::env::var_os("XDG_RUNTIME_DIR").is_some() {
            assert!(dir.ends_with(crate::APP_ID), "{}", dir.display());
        } else {
            assert_eq!(dir, data);
        }
    }
}
