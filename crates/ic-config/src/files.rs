//! File system helpers: private directories, reading text, and atomic,
//! durable replacement of a file with a backup of the old contents.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use tempfile::NamedTempFile;

use crate::error::ConfigError;

/// How many symbolic links a chain may have, like Linux's `MAXSYMLINKS`.
const MAX_LINKS: usize = 40;

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
///
/// A symbolic link whose target doesn't exist, or a path below such a
/// link, is an error rather than a missing file: the file is probably on a
/// volume that isn't mounted yet or in a dotfiles repository that moved,
/// and treating it as missing would hide that.
pub(crate) fn read_text(path: &Path) -> Result<Option<String>, ConfigError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return match dangling_link(path) {
                Some(link) => Err(dangling_link_error(&link)),
                None => Ok(None),
            };
        }
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
/// Existing directories keep their permissions. A symbolic link whose
/// target doesn't exist, at `dir` or above it, is an error: creating the
/// directory would replace or bypass the link.
pub(crate) fn create_private_dir(dir: &Path) -> Result<(), ConfigError> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(link) = dangling_link(dir) {
        return Err(dangling_link_error(&link));
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
/// 2. if `keep_previous` says the previous contents deserve it (this
///    version can't read them), copy them to a new file next to `backup`
///    that later saves never replace (`<name>.unreadable-<unix seconds>`)
///    and leave `backup` alone: it holds the last contents that could be
///    read, which the user may still want to restore;
/// 3. otherwise copy the previous contents to `backup` the same way
///    (temporary file, sync, rename), keeping exactly one backup;
/// 4. rename the temporary file over the target (atomic on POSIX), then sync
///    the directory so the rename itself is durable.
///
/// If `path` is a symbolic link, the file at the end of the chain of links
/// is replaced and the link is kept (dotfile managers link settings into a
/// repository), also when that file doesn't exist yet. Directories are
/// only created when no link is involved: a link into a missing directory
/// (a volume that isn't mounted) is an error. The result is readable only
/// by the user on Unix. When the target already has exactly these contents
/// nothing is written, which keeps the backup one real change behind.
///
/// `checkpoint` runs after each [`Step`]; an error there aborts the save
/// like a failure of the step itself would.
pub(crate) fn write_atomic(
    path: &Path,
    backup: &Path,
    contents: &[u8],
    keep_previous: impl FnOnce(&[u8]) -> bool,
    mut checkpoint: impl FnMut(Step) -> io::Result<()>,
) -> Result<(), ConfigError> {
    let target = follow_links(path).map_err(|error| ConfigError::io("resolving", path, error))?;
    let dir = parent_dir(&target);
    if target == path {
        create_private_dir(dir)?;
    } else if !dir.is_dir() {
        return Err(dangling_link_error(path));
    }
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

    let backup_dir = parent_dir(backup);
    if let Some(previous) = &previous {
        if keep_previous(previous) {
            // An unreadable file must not replace the backup: "start
            // fresh" after a bad hand edit would lose the last good
            // settings the recovery screen had just offered to restore.
            let kept = keep_copy(path, backup_dir, previous)?;
            tracing::warn!(
                path = %kept.display(),
                "kept a copy of the replaced settings file, which this version can't read"
            );
        } else {
            let backup_temp = write_temp(backup_dir, backup, previous)?;
            backup_temp
                .persist(backup)
                .map_err(|error| ConfigError::io("replacing", backup, error.error))?;
        }
        if backup_dir != dir {
            sync_dir(backup_dir);
        }
    }
    checkpoint(Step::BackedUp).map_err(|error| ConfigError::io("replacing", &target, error))?;

    temp.persist(&target)
        .map_err(|error| ConfigError::io("replacing", &target, error.error))?;
    sync_dir(dir);
    Ok(())
}

/// The file `path` refers to: `path` itself, or for a symbolic link the
/// end of its chain of links, which may not exist. Relative targets are
/// resolved against the directory of the link that holds them.
fn follow_links(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                let target = fs::read_link(&current)?;
                current = parent_dir(&current).join(target);
            }
            // Nothing there (or a file where a directory should be): no
            // link to follow, and writing reports what is wrong.
            Ok(_) => return Ok(current),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(current);
            }
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

/// The deepest of `path` and its ancestors that is a symbolic link whose
/// target doesn't exist.
fn dangling_link(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
        .find(|ancestor| {
            fs::symlink_metadata(ancestor).is_ok_and(|metadata| metadata.file_type().is_symlink())
                && fs::metadata(ancestor)
                    .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        })
        .map(Path::to_path_buf)
}

/// The error for the symbolic link `link`, whose target doesn't exist.
fn dangling_link_error(link: &Path) -> ConfigError {
    let target = follow_links(link).unwrap_or_else(|_| link.to_path_buf());
    ConfigError::io(
        "following the symbolic link",
        link,
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("its target {} does not exist", target.display()),
        ),
    )
}

/// The directory a file is in; `.` for a bare file name.
fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// Copies `contents` into `dir` under a name no other file has and no save
/// replaces: `<name of path>.unreadable-<unix seconds>`, with `-2`, `-3`, …
/// appended when that is taken. Returns the copy's path.
fn keep_copy(path: &Path, dir: &Path, contents: &[u8]) -> Result<PathBuf, ConfigError> {
    let name = path.file_name().unwrap_or_else(|| OsStr::new("settings"));
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let copy = (1..=1000_u32)
        .map(|number| {
            let mut file_name = name.to_os_string();
            file_name.push(format!(".unreadable-{seconds}"));
            if number > 1 {
                file_name.push(format!("-{number}"));
            }
            dir.join(file_name)
        })
        .find(|candidate| {
            fs::symlink_metadata(candidate)
                .is_err_and(|error| error.kind() == io::ErrorKind::NotFound)
        })
        .ok_or_else(|| {
            ConfigError::io(
                "finding a free name for a copy of the unreadable settings in",
                dir,
                io::Error::from(io::ErrorKind::AlreadyExists),
            )
        })?;
    write_temp(dir, &copy, contents)?
        .persist(&copy)
        .map_err(|error| {
            ConfigError::io(
                "keeping a copy of the unreadable settings as",
                &copy,
                error.error,
            )
        })?;
    Ok(copy)
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

    fn keep_nothing(_: &[u8]) -> bool {
        false
    }

    #[test]
    fn writes_a_new_file_without_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
        assert_eq!(read(&path), "one");
        assert!(!backup.exists());
        assert_eq!(entries(dir.path()), ["config.toml"]);
    }

    #[test]
    fn keeps_the_previous_contents_as_the_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap();
        assert_eq!(
            (read(&path).as_str(), read(&backup).as_str()),
            ("two", "one")
        );
        write_atomic(&path, &backup, b"three", keep_nothing, no_checkpoint).unwrap();
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
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap();
        let mut steps = Vec::new();
        write_atomic(&path, &backup, b"two", keep_nothing, |step| {
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
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap();

        let error = write_atomic(&path, &backup, b"three", keep_nothing, |step| match step {
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
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
        write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap();

        let mut steps = Vec::new();
        let result = write_atomic(&path, &backup, b"three", keep_nothing, |step| {
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
        write_atomic(&path, &backup, b"three", keep_nothing, no_checkpoint).unwrap();
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
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
        // A directory where the backup should go makes the backup rename fail.
        fs::create_dir(&backup).unwrap();
        fs::write(backup.join("keep"), b"x").unwrap();

        let error = write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap_err();
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
    fn readable_previous_contents_are_only_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        let mut asked = 0;
        // Nothing to judge for a new file, or for unchanged contents.
        for contents in [b"one", b"one"] {
            write_atomic(
                &path,
                &backup,
                contents,
                |_: &[u8]| {
                    asked += 1;
                    false
                },
                no_checkpoint,
            )
            .unwrap();
        }
        assert_eq!(asked, 0);
        write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap();
        assert_eq!(entries(dir.path()), ["config.toml", "config.toml.bak"]);
    }

    #[test]
    fn stale_temporary_files_do_not_matter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = dir.path().join("config.toml.bak");
        // What a crash between writing and renaming leaves behind.
        fs::write(dir.path().join(".config.toml.Ab12Cd.tmp"), b"half").unwrap();
        write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
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
            keep_nothing,
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
            keep_nothing,
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
            write_atomic(&path, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
            assert_eq!(mode(&path), 0o600);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            write_atomic(&path, &backup, b"two", keep_nothing, no_checkpoint).unwrap();
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
                keep_nothing,
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

            write_atomic(&link, &backup, b"two", keep_nothing, no_checkpoint).unwrap();
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

        fn is_link(path: &Path) -> bool {
            fs::symlink_metadata(path).unwrap().file_type().is_symlink()
        }

        #[test]
        fn links_to_files_that_do_not_exist_yet_are_written_through() {
            let dir = tempfile::tempdir().unwrap();
            let dotfiles = dir.path().join("dotfiles");
            fs::create_dir(&dotfiles).unwrap();
            let link = dir.path().join("config.toml");
            // A relative target, resolved against the link's directory.
            symlink("dotfiles/icygui.toml", &link).unwrap();
            let backup = dir.path().join("config.toml.bak");
            write_atomic(&link, &backup, b"one", keep_nothing, no_checkpoint).unwrap();
            assert!(is_link(&link));
            assert_eq!(read(&dotfiles.join("icygui.toml")), "one");
            assert!(!backup.exists());
        }

        #[test]
        fn chains_of_links_are_followed_to_the_end() {
            let dir = tempfile::tempdir().unwrap();
            let real = dir.path().join("real.toml");
            fs::write(&real, b"one").unwrap();
            let middle = dir.path().join("middle.toml");
            symlink(&real, &middle).unwrap();
            let link = dir.path().join("config.toml");
            symlink(&middle, &link).unwrap();
            let backup = dir.path().join("config.toml.bak");
            write_atomic(&link, &backup, b"two", keep_nothing, no_checkpoint).unwrap();
            assert!(is_link(&link) && is_link(&middle));
            assert_eq!(read(&real), "two");
            assert_eq!(read(&backup), "one");
        }

        #[test]
        fn links_into_missing_directories_are_left_alone() {
            // A dotfiles repository that moved, or a volume that isn't
            // mounted yet: neither the link nor the missing directory may
            // be replaced by a new file.
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("config.toml");
            let target = dir.path().join("missing/target.toml");
            symlink(&target, &link).unwrap();
            let error = write_atomic(
                &link,
                &dir.path().join("config.toml.bak"),
                b"one",
                keep_nothing,
                no_checkpoint,
            )
            .unwrap_err();
            let message = error.to_string();
            assert!(
                message.starts_with(&format!(
                    "following the symbolic link {}: its target {} does not exist",
                    link.display(),
                    target.display()
                )),
                "{message}"
            );
            assert!(is_link(&link));
            assert!(!dir.path().join("missing").exists());
            assert_eq!(entries(dir.path()), ["config.toml"]);
        }

        #[test]
        fn dangling_links_are_not_missing_files() {
            let dir = tempfile::tempdir().unwrap();
            let link = dir.path().join("config.toml");
            let target = dir.path().join("dotfiles/icygui.toml");
            symlink(&target, &link).unwrap();
            match read_text(&link) {
                Err(ConfigError::Io { action, path, .. }) => {
                    assert_eq!(action, "following the symbolic link");
                    assert_eq!(path, link);
                }
                other => panic!("expected an I/O error, got {other:?}"),
            }

            // The same for a file below a dangling directory link (GNU
            // Stow links whole directories).
            let config_dir = dir.path().join("icygui");
            symlink(dir.path().join("stow/icygui"), &config_dir).unwrap();
            let below = config_dir.join("config.toml");
            match read_text(&below) {
                Err(ConfigError::Io { path, .. }) => assert_eq!(path, config_dir),
                other => panic!("expected an I/O error, got {other:?}"),
            }
            match create_private_dir(&config_dir) {
                Err(ConfigError::Io { action, path, .. }) => {
                    assert_eq!(
                        (action, path),
                        ("following the symbolic link", config_dir.clone())
                    );
                }
                other => panic!("expected an I/O error, got {other:?}"),
            }
            assert!(
                write_atomic(
                    &below,
                    &config_dir.join("config.toml.bak"),
                    b"one",
                    keep_nothing,
                    no_checkpoint
                )
                .is_err()
            );
            assert!(is_link(&config_dir));
            assert!(!dir.path().join("stow").exists());
        }

        #[test]
        fn copies_of_unreadable_files_are_private_and_never_replaced() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            let backup = dir.path().join("config.toml.bak");
            fs::write(&path, b"broken").unwrap();
            let mut offered = Vec::new();
            write_atomic(
                &path,
                &backup,
                b"one",
                |previous: &[u8]| {
                    offered.push(previous.to_vec());
                    true
                },
                no_checkpoint,
            )
            .unwrap();
            assert_eq!(offered, [b"broken".to_vec()]);
            fs::write(&path, b"broken again").unwrap();
            write_atomic(&path, &backup, b"two", |_: &[u8]| true, no_checkpoint).unwrap();

            let copies: Vec<String> = entries(dir.path())
                .into_iter()
                .filter(|name| name.starts_with("config.toml.unreadable-"))
                .collect();
            assert_eq!(copies.len(), 2, "{copies:?}");
            let mut contents: Vec<String> = copies
                .iter()
                .map(|name| read(&dir.path().join(name)))
                .collect();
            contents.sort();
            assert_eq!(contents, ["broken", "broken again"]);
            for name in &copies {
                assert_eq!(mode(&dir.path().join(name)), 0o600);
            }
            assert_eq!(read(&path), "two");
            // Unreadable contents never become the backup.
            assert!(!backup.exists());
        }

        #[test]
        fn an_unreadable_file_leaves_the_last_good_backup_alone() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config.toml");
            let backup = dir.path().join("config.toml.bak");
            fs::write(&path, b"broken").unwrap();
            fs::write(&backup, b"good").unwrap();
            write_atomic(&path, &backup, b"fresh", |_: &[u8]| true, no_checkpoint).unwrap();
            assert_eq!(read(&path), "fresh");
            assert_eq!(read(&backup), "good");
            // The next ordinary save backs up the readable file again.
            write_atomic(&path, &backup, b"edited", keep_nothing, no_checkpoint).unwrap();
            assert_eq!(read(&backup), "fresh");
        }
    }
}
