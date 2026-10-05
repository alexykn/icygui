//! File system helpers: private directories, reading text, and atomic,
//! durable replacement of a file with a backup of the old contents.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::error::ConfigError;

/// Points where [`write_atomic`] can be interrupted. Tests inject failures
/// there to check that an interrupted save leaves the old file in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// The new contents are in a synced temporary file next to the target.
    TempWritten,
    /// The previous contents are in the backup file.
    BackedUp,
}

/// Reads a UTF-8 text file; `None` if it doesn't exist.
pub(crate) fn read_text(path: &Path) -> Result<Option<String>, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ConfigError::io("reading", path, error)),
    };
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| ConfigError::Parse {
            message: format!(
                "{} is not UTF-8 text (invalid byte at offset {})",
                path.display(),
                error.utf8_error().valid_up_to()
            ),
        })
}

/// Creates `dir` and missing parents, readable only by the user on Unix.
/// Existing directories keep their permissions.
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), ConfigError> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|error| ConfigError::io("creating directory", dir, error))
}

/// Replaces `path` with `contents` so that a crash or power loss at any
/// moment leaves either the old or the new file, never a mix:
///
/// 1. write the contents to a temporary file in the target's directory and
///    sync it to disk;
/// 2. copy the previous contents to `backup` the same way (temporary file,
///    sync, rename), keeping exactly one backup;
/// 3. rename the temporary file over the target (atomic on POSIX), then sync
///    the directory so the rename itself is durable.
///
/// If `path` is a symbolic link, the file it points to is replaced and the
/// link is kept (dotfile managers link settings into a repository). The
/// result is readable only by the user on Unix. When the target already
/// has exactly these contents nothing is written, which keeps the backup
/// one real change behind.
///
/// `checkpoint` runs after each [`Step`]; an error there aborts the save
/// like a failure of the step itself would.
pub(crate) fn write_atomic(
    path: &Path,
    backup: &Path,
    contents: &[u8],
    mut checkpoint: impl FnMut(Step) -> io::Result<()>,
) -> Result<(), ConfigError> {
    let target = resolve_symlink(path);
    let dir = parent_dir(&target);
    create_private_dir(dir)?;
    let previous = match fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(ConfigError::io("reading", &target, error)),
    };
    if previous.as_deref() == Some(contents) {
        // Nothing to write. Failing to tighten the permissions of a file
        // that is managed elsewhere (read-only) must not fail the save.
        if let Err(error) = restrict_permissions(&target) {
            tracing::warn!(%error, "could not make the settings file private");
        }
        return Ok(());
    }

    let temp = write_temp(dir, &target, contents)?;
    checkpoint(Step::TempWritten)
        .map_err(|error| ConfigError::io("writing", temp.path(), error))?;

    if let Some(previous) = &previous {
        let backup_temp = write_temp(parent_dir(backup), backup, previous)?;
        backup_temp
            .persist(backup)
            .map_err(|error| ConfigError::io("replacing", backup, error.error))?;
    }
    checkpoint(Step::BackedUp).map_err(|error| ConfigError::io("replacing", &target, error))?;

    temp.persist(&target)
        .map_err(|error| ConfigError::io("replacing", &target, error.error))?;
    sync_dir(dir);
    Ok(())
}

/// The file a symbolic link points to, or `path` itself. A dangling link is
/// replaced like a missing file.
fn resolve_symlink(path: &Path) -> PathBuf {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        }
        _ => path.to_path_buf(),
    }
}

/// The directory a file is in; `.` for a bare file name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Writes `contents` to a new, synced, user-only temporary file in `dir`,
/// named after `target` (`.config.toml.XXXXXX.tmp`). The file is deleted
/// if it is dropped without being persisted.
fn write_temp(dir: &Path, target: &Path, contents: &[u8]) -> Result<NamedTempFile, ConfigError> {
    let mut prefix = OsString::from(".");
    prefix.push(target.file_name().unwrap_or_else(|| OsStr::new("settings")));
    prefix.push(".");
    let mut temp = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|error| ConfigError::io("creating a temporary file in", dir, error))?;
    restrict_file(temp.as_file())
        .map_err(|error| ConfigError::io("setting permissions on", temp.path(), error))?;
    temp.write_all(contents)
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|error| ConfigError::io("writing", temp.path(), error))?;
    Ok(temp)
}

/// Makes an existing file readable and writable by the user only.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), ConfigError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = fs::metadata(path)
        .map_err(|error| ConfigError::io("reading permissions of", path, error))?
        .permissions()
        .mode();
    if mode & 0o777 == 0o600 {
        return Ok(());
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|error| ConfigError::io("setting permissions on", path, error))
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), ConfigError> {
    Ok(())
}

/// Makes an open file readable and writable by the user only, regardless
/// of the umask.
#[cfg(unix)]
fn restrict_file(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_file: &File) -> io::Result<()> {
    Ok(())
}

/// Makes a rename in `dir` durable. Best effort: the data itself is already
/// synced, and some file systems don't support syncing directories.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Err(error) = File::open(dir).and_then(|handle| handle.sync_all()) {
        tracing::debug!(%error, dir = %dir.display(), "could not sync directory");
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).unwrap()
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[expect(
        clippy::unnecessary_wraps,
        reason = "has the signature write_atomic expects of a checkpoint"
    )]
    fn no_checkpoint(_: Step) -> io::Result<()> {
        Ok(())
    }

    #[test]
    fn writes_a_new_file_without_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        assert_eq!(read(&path), "one");
        assert!(!backup.exists());
        assert_eq!(entries(dir.path()), ["config.toml"]);
    }

    #[test]
    fn keeps_the_previous_contents_as_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", no_checkpoint).unwrap();
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("two", "one")
        );
        write_atomic(&path, &backup, b"three", no_checkpoint).unwrap();
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("three", "two")
        );
        assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
    }

    #[test]
    fn unchanged_contents_are_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", no_checkpoint).unwrap();
        let mut steps = Vec::new();
        write_atomic(&path, &backup, b"two", |step| {
            steps.push(step);
            Ok(())
        })
        .unwrap();
        assert!(steps.is_empty(), "nothing was written");
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("two", "one")
        );
    }

    #[test]
    fn interruption_before_the_backup_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", no_checkpoint).unwrap();

        let error = write_atomic(&path, &backup, b"three", |step| match step {
            Step::TempWritten => Err(io::Error::other("simulated crash")),
            Step::BackedUp => Ok(()),
        })
        .unwrap_err();
        assert!(error.to_string().contains("simulated crash"), "{error}");
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("two", "one")
        );
        assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
    }

    #[test]
    fn interruption_before_the_rename_keeps_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", no_checkpoint).unwrap();

        let mut steps = Vec::new();
        let result = write_atomic(&path, &backup, b"three", |step| {
            steps.push(step);
            match step {
                Step::TempWritten => Ok(()),
                Step::BackedUp => Err(io::Error::other("simulated crash")),
            }
        });
        assert!(result.is_err());
        assert_eq!(steps, [Step::TempWritten, Step::BackedUp]);
        // The backup already holds the current file; the file itself is intact.
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("two", "two")
        );
        assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);

        // The next save works normally.
        write_atomic(&path, &backup, b"three", no_checkpoint).unwrap();
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("three", "two")
        );
    }

    #[test]
    fn a_failing_backup_leaves_the_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        // A directory where the backup should go makes the backup rename fail.
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("keep"), b"x").unwrap();

        let error = write_atomic(&path, &backup, b"two", no_checkpoint).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Io {
                    action: "replacing",
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(read(&path), "one");
        assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
    }

    #[test]
    fn stale_temporary_files_do_not_matter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        // What a crash between writing and renaming leaves behind.
        fs::write(dir.path().join(".config.toml.Ab12Cd.tmp"), b"half").unwrap();
        write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
        assert_eq!(read(&path), "one");
    }

    #[test]
    fn creates_missing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/config.toml");
        write_atomic(
            &path,
            &dir.path().join("a/b/config.toml.bak"),
            b"one",
            no_checkpoint,
        )
        .unwrap();
        assert_eq!(read(&path), "one");
    }

    #[test]
    fn reports_a_parent_that_is_a_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file"), b"x").unwrap();
        let path = dir.path().join("file/config.toml");
        let error = write_atomic(
            &path,
            &dir.path().join("file/config.toml.bak"),
            b"one",
            no_checkpoint,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Io {
                    action: "creating directory",
                    ..
                }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn reads_text_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        assert_eq!(read_text(&path).unwrap(), None);
        fs::write(&path, "version = 1\n").unwrap();
        assert_eq!(read_text(&path).unwrap().as_deref(), Some("version = 1\n"));
        fs::write(&path, b"version = \xff\n").unwrap();
        match read_text(&path) {
            Err(ConfigError::Parse { message }) => {
                assert!(
                    message.ends_with("is not UTF-8 text (invalid byte at offset 10)"),
                    "{message}"
                );
            }
            other => panic!("expected a parse error, got {other:?}"),
        }
        assert!(matches!(
            read_text(dir.path()),
            Err(ConfigError::Io {
                action: "reading",
                ..
            })
        ));
    }

    #[cfg(unix)]
    mod unix {
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        use super::*;

        fn mode(path: &Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        #[test]
        fn files_are_private() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            let backup = dir.path().join("config.toml.bak");
            write_atomic(&path, &backup, b"one", no_checkpoint).unwrap();
            assert_eq!(mode(&path), 0o600);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            write_atomic(&path, &backup, b"two", no_checkpoint).unwrap();
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&backup), 0o600);
        }

        #[test]
        fn unchanged_files_are_still_made_private() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            fs::write(&path, b"one").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            write_atomic(
                &path,
                &dir.path().join("config.toml.bak"),
                b"one",
                no_checkpoint,
            )
            .unwrap();
            assert_eq!(mode(&path), 0o600);
        }

        #[test]
        fn new_directories_are_private() {
            let dir = tempfile::tempdir().unwrap();
            let nested = dir.path().join("a/b");
            create_private_dir(&nested).unwrap();
            assert_eq!(mode(&nested), 0o700);
            assert_eq!(mode(&dir.path().join("a")), 0o700);
            // Existing directories keep their mode.
            fs::set_permissions(&nested, fs::Permissions::from_mode(0o755)).unwrap();
            create_private_dir(&nested).unwrap();
            assert_eq!(mode(&nested), 0o755);
        }

        #[test]
        fn symbolic_links_are_kept() {
            let dir = tempfile::tempdir().unwrap();
            let dotfiles = dir.path().join("dotfiles");
            fs::create_dir(&dotfiles).unwrap();
            let real = dotfiles.join("icygui.toml");
            fs::write(&real, b"one").unwrap();
            let link = dir.path().join("config.toml");
            symlink(&real, &link).unwrap();
            let backup = dir.path().join("config.toml.bak");

            write_atomic(&link, &backup, b"two", no_checkpoint).unwrap();
            assert!(
                fs::symlink_metadata(&link)
                    .unwrap()
                    .file_type()
                    .is_symlink()
            );
            assert_eq!(read(&real), "two");
            assert_eq!(read(&backup), "one");
            assert_eq!(
                entries(&dotfiles),
                ["icygui.toml"],
                "no backup or temp files next to the target"
            );
        }

        #[test]
        fn dangling_links_are_replaced() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("config.toml");
            symlink(dir.path().join("missing/target.toml"), &link).unwrap();
            write_atomic(
                &link,
                &dir.path().join("config.toml.bak"),
                b"one",
                no_checkpoint,
            )
            .unwrap();
            assert!(fs::symlink_metadata(&link).unwrap().is_file());
            assert_eq!(read(&link), "one");
        }
    }
}
