//! Logging (OPS-05): to stderr and to a size-rotated file in the log
//! directory, `icygui.log` plus `icygui.log.1` … `icygui.log.4`, at most
//! 10 MiB each, readable by the user only.
//!
//! Nothing the app logs carries a secret: passwords stay in the keychain
//! and never pass through this crate's log calls, the core redacts
//! credentials, and the mock's users print as `<redacted>`.
//!
//! `RUST_LOG` sets the filter (default `info`, with chatty GPU and D-Bus
//! crates at `warn`). Panics are logged before the default hook prints
//! them, so a crash leaves its message in the file.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

/// The log file's name in the log directory.
pub(crate) const LOG_FILE: &str = "icygui.log";
/// A log file is rotated when the next line would take it past this.
const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// How many rotated files are kept besides the current one.
const KEEP: usize = 4;
/// The filter without `RUST_LOG`.
const DEFAULT_FILTER: &str = "info,naga=warn,wgpu_core=warn,wgpu_hal=warn,zbus=warn,blade=warn";

/// Sets up logging to stderr and, when `log_dir` is given and writable, to
/// the rotating log file there. Returns the log file's path, if any.
pub(crate) fn init(log_dir: Option<&Path>) -> Option<PathBuf> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let mut file_error = None;
    let file = log_dir.and_then(|dir| {
        let path = dir.join(LOG_FILE);
        match RotatingFile::open(path.clone(), MAX_BYTES, KEEP) {
            Ok(file) => Some((file, path)),
            Err(error) => {
                file_error = Some((path, error));
                None
            }
        }
    });
    let path = file.as_ref().map(|(_, path)| path.clone());
    let file_layer = file.map(|(file, _)| {
        tracing_subscriber::fmt::layer()
            .with_ansi(false)
            .with_writer(file)
    });
    let stderr_layer = tracing_subscriber::fmt::layer().with_writer(io::stderr);
    let installed = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer)
        .try_init();
    if installed.is_err() {
        // Only tests install a subscriber of their own first.
        return None;
    }
    if let Some((path, error)) = file_error {
        tracing::warn!(path = %path.display(), %error, "logging to stderr only: the log file can't be opened");
    }
    install_panic_hook();
    path
}

/// Logs panics (message and location) before the default hook runs.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}", location.file(), location.line()))
            .unwrap_or_default();
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(ToString::to_string)
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_default();
        tracing::error!(%location, %message, "icygui panicked");
        default(info);
    }));
}

/// A log file that is rotated by size: when a write would take it past
/// `max_bytes`, `name.N` becomes `name.N+1` (the oldest is dropped),
/// `name` becomes `name.1`, and a new `name` is started. Every write goes
/// to the file at once, so nothing is lost when the app crashes.
#[derive(Debug)]
pub(crate) struct RotatingFile {
    state: Mutex<FileState>,
}

#[derive(Debug)]
struct FileState {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: File,
    size: u64,
}

impl RotatingFile {
    /// Opens (appends to) the log file at `path`, creating it user-only.
    ///
    /// # Errors
    ///
    /// The file can't be opened or created.
    pub(crate) fn open(path: PathBuf, max_bytes: u64, keep: usize) -> io::Result<Self> {
        let file = open_append(&path)?;
        let size = file.metadata()?.len();
        Ok(Self {
            state: Mutex::new(FileState {
                path,
                max_bytes,
                keep,
                file,
                size,
            }),
        })
    }
}

impl FileState {
    /// Moves the files one step down and starts a new one.
    fn rotate(&mut self) -> io::Result<()> {
        for index in (1..self.keep).rev() {
            let from = numbered(&self.path, index);
            if from.exists() {
                fs::rename(&from, numbered(&self.path, index + 1))?;
            }
        }
        if self.keep > 0 {
            fs::rename(&self.path, numbered(&self.path, 1))?;
        } else {
            fs::remove_file(&self.path)?;
        }
        self.file = open_append(&self.path)?;
        self.size = 0;
        Ok(())
    }
}

/// `name.N`.
fn numbered(path: &Path, index: usize) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".{index}"));
    PathBuf::from(name)
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

impl Write for &RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let length = u64::try_from(buf.len()).unwrap_or(u64::MAX);
        if state.size > 0 && state.size.saturating_add(length) > state.max_bytes {
            // A failed rotation keeps appending to the current file rather
            // than losing lines.
            let _ = state.rotate();
        }
        state.file.write_all(buf)?;
        state.size = state.size.saturating_add(length);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .file
            .flush()
    }
}

impl<'a> MakeWriter<'a> for RotatingFile {
    type Writer = &'a RotatingFile;

    fn make_writer(&'a self) -> Self::Writer {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap_or_default()
    }

    #[test]
    fn appends_and_rotates_by_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let file = RotatingFile::open(path.clone(), 20, 2).unwrap();
        let mut writer = &file;
        writer.write_all(b"first line\n").unwrap();
        writer.write_all(b"second\n").unwrap();
        assert_eq!(read(&path), "first line\nsecond\n");
        // Past 20 bytes: rotated before the write.
        writer.write_all(b"third line\n").unwrap();
        assert_eq!(read(&path), "third line\n");
        assert_eq!(read(&numbered(&path, 1)), "first line\nsecond\n");
        writer.write_all(b"fourth line\n").unwrap();
        writer.write_all(b"fifth line!\n").unwrap();
        assert_eq!(read(&path), "fifth line!\n");
        assert_eq!(read(&numbered(&path, 1)), "fourth line\n");
        assert_eq!(read(&numbered(&path, 2)), "third line\n");
        assert!(!numbered(&path, 3).exists(), "only two are kept");
    }

    #[test]
    fn a_line_longer_than_the_limit_still_goes_in() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let file = RotatingFile::open(path.clone(), 4, 1).unwrap();
        let mut writer = &file;
        writer.write_all(b"a very long line\n").unwrap();
        assert_eq!(read(&path), "a very long line\n");
    }

    #[test]
    fn reopening_continues_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        {
            let file = RotatingFile::open(path.clone(), 1_000, 2).unwrap();
            (&file).write_all(b"before\n").unwrap();
        }
        let file = RotatingFile::open(path.clone(), 1_000, 2).unwrap();
        (&file).write_all(b"after\n").unwrap();
        assert_eq!(read(&path), "before\nafter\n");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let _file = RotatingFile::open(path.clone(), 1_000, 2).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn the_subscriber_writes_formatted_lines_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOG_FILE);
        let file = RotatingFile::open(path.clone(), 1_000_000, 2).unwrap();
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer(file)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(environment = "prod-cluster", "connected");
        });
        let text = read(&path);
        assert!(text.contains("connected"), "{text}");
        assert!(text.contains("environment=\"prod-cluster\""), "{text}");
        assert!(!text.contains('\u{1b}'), "no colour codes in the file");
    }
}
