//! The files and folders behind the settings: *edit in settings file* and
//! *edit keymap file* open them in the default editor, *open folder* opens
//! the log and config folders in the file manager, and the advanced page
//! says how much the logs hold.

use std::path::{Path, PathBuf};

use gpui::App;

/// Opens `path` with the system's default application (an editor for a
/// file, the file manager for a folder). Tests record it instead.
pub(crate) fn open(path: &Path, cx: &App) {
    tracing::info!(path = %path.display(), "opening");
    #[cfg(test)]
    {
        let _ = cx;
        OPENED.with(|opened| opened.borrow_mut().push(path.to_path_buf()));
    }
    #[cfg(not(test))]
    cx.open_with_system(path);
}

#[cfg(test)]
thread_local! {
    /// What [`open`] opened, for tests.
    pub(crate) static OPENED: std::cell::RefCell<Vec<PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// `path` with the home directory written as `~` (`~/.config/icygui`).
pub(crate) fn tilde(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// What the log folder holds: how many log files, how many bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LogSummary {
    /// Log files (`icygui.log` and its rotated copies).
    pub(crate) files: usize,
    /// Their size together.
    pub(crate) bytes: u64,
}

impl LogSummary {
    /// Reads the log folder (off the UI thread: a slow disk shouldn't
    /// hold the window up).
    pub(crate) fn of(dir: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Self::default();
        };
        let mut summary = Self::default();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(crate::logging::LOG_FILE) {
                continue;
            }
            if let Ok(metadata) = entry.metadata()
                && metadata.is_file()
            {
                summary.files += 1;
                summary.bytes += metadata.len();
            }
        }
        summary
    }

    /// `3 files, 4.1 MiB`.
    pub(crate) fn text(self) -> String {
        let files = match self.files {
            1 => "1 file".to_owned(),
            count => format!("{count} files"),
        };
        format!("{files}, {}", size_text(self.bytes))
    }
}

/// Bytes as `812 B`, `14 KiB`, `4.1 MiB` (binary units, as the log's
/// limits are set).
pub(crate) fn size_text(bytes: u64) -> String {
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    #[expect(
        clippy::cast_precision_loss,
        reason = "a log folder's size is far below 2^52 bytes"
    )]
    let megabytes = bytes as f64 / MB as f64;
    match bytes {
        0..KB => format!("{bytes} B"),
        KB..MB => format!("{} KiB", bytes / KB),
        _ if bytes.is_multiple_of(MB) => format!("{} MiB", bytes / MB),
        _ => format!("{megabytes:.1} MiB"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_like_a_file_manager() {
        assert_eq!(size_text(812), "812 B");
        assert_eq!(size_text(14_400), "14 KiB");
        assert_eq!(size_text(4_300_000), "4.1 MiB");
        assert_eq!(size_text(50 << 20), "50 MiB");
    }

    #[test]
    fn the_summary_counts_only_log_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("icygui.log"), vec![b'x'; 2_000]).unwrap();
        std::fs::write(dir.path().join("icygui.log.1"), vec![b'x'; 3_000]).unwrap();
        std::fs::write(dir.path().join("other.txt"), b"no").unwrap();
        let summary = LogSummary::of(dir.path());
        assert_eq!(
            summary,
            LogSummary {
                files: 2,
                bytes: 5_000
            }
        );
        assert_eq!(summary.text(), "2 files, 4 KiB");
        assert_eq!(
            LogSummary::of(&dir.path().join("missing")),
            LogSummary::default()
        );
    }

    #[test]
    fn the_home_directory_reads_as_a_tilde() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        assert_eq!(tilde(&home.join(".config/icygui")), "~/.config/icygui");
        assert_eq!(tilde(&home), "~");
        assert_eq!(tilde(Path::new("/etc/icygui")), "/etc/icygui");
    }
}
