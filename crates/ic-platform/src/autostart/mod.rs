//! Launch at login (PLAN.md D4, BG-03).
//!
//! - **macOS:** a launch agent, `~/Library/LaunchAgents/<app_id>.plist`, with
//!   `RunAtLoad` and the program arguments `[exe, "--background"]`.
//! - **Linux:** an XDG autostart entry,
//!   `$XDG_CONFIG_HOME/autostart/<app_id>.desktop` (default
//!   `~/.config/autostart`), whose `Exec` is the quoted executable path
//!   followed by `--background`.
//!
//! Both are plain files: enabling writes the file (atomically, and only if
//! its contents change), disabling removes it. Nothing is started or
//! stopped right away; the entry takes effect at the next login.

mod desktop;
mod launch_agent;

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::PlatformError;

/// The argument the login entry passes, so the app starts in the tray
/// without opening its window.
pub const BACKGROUND_ARG: &str = "--background";

/// Turns launch at login on or off for the app.
///
/// - `app_id`: reverse-DNS application id (`io.github.alexykn.icygui`); it
///   names the file and, on macOS, the launchd job.
/// - `app_name`: shown by the desktop's autostart settings (Linux).
/// - `exe`: absolute path of the program to start. Pass the path users
///   launch (for an `AppImage`, the `AppImage` itself), not a temporary one.
///
/// Enabling again rewrites the entry if `exe` changed and does nothing
/// otherwise; disabling an entry that doesn't exist succeeds.
///
/// # Errors
///
/// - [`PlatformError::InvalidArgument`]: `app_id` isn't a plain reverse-DNS
///   id, or (when enabling) `exe` isn't an absolute UTF-8 path or a name or
///   path contains control characters.
/// - [`PlatformError::NoHomeDirectory`]: the home directory is unknown.
/// - [`PlatformError::Io`]: the entry could not be written or removed.
/// - [`PlatformError::Unsupported`]: neither macOS nor a freedesktop system.
pub fn set_enabled(
    enabled: bool,
    app_id: &str,
    app_name: &str,
    exe: &Path,
) -> Result<(), PlatformError> {
    let format = Format::current().ok_or(PlatformError::Unsupported)?;
    let dir = format.system_dir()?;
    set_enabled_in(format, &dir, enabled, app_id, app_name, exe)
}

/// Whether launch at login is on: the entry exists and isn't disabled
/// (`Disabled` in a launch agent, `Hidden=true` or
/// `X-GNOME-Autostart-enabled=false` in an autostart entry). Any problem
/// reading it counts as off.
pub fn is_enabled(app_id: &str) -> bool {
    let Some(format) = Format::current() else {
        return false;
    };
    format
        .system_dir()
        .is_ok_and(|dir| is_enabled_in(format, &dir, app_id))
}

/// The kind of login entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    /// A launchd property list in `~/Library/LaunchAgents` (macOS).
    LaunchAgent,
    /// A desktop entry in `$XDG_CONFIG_HOME/autostart` (freedesktop).
    XdgAutostart,
}

impl Format {
    /// The format of the platform this was built for.
    fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::LaunchAgent)
        } else if cfg!(all(
            unix,
            not(any(target_os = "ios", target_os = "android"))
        )) {
            Some(Self::XdgAutostart)
        } else {
            None
        }
    }

    /// The directory the entry goes into, from the environment.
    fn system_dir(self) -> Result<PathBuf, PlatformError> {
        let home = std::env::home_dir();
        match self {
            Self::LaunchAgent => launch_agents_dir(home),
            Self::XdgAutostart => xdg_autostart_dir(std::env::var_os("XDG_CONFIG_HOME"), home),
        }
        .ok_or(PlatformError::NoHomeDirectory)
    }

    fn file_name(self, app_id: &str) -> String {
        match self {
            Self::LaunchAgent => format!("{app_id}.plist"),
            Self::XdgAutostart => format!("{app_id}.desktop"),
        }
    }

    fn render(self, app_id: &str, app_name: &str, exe: &str) -> String {
        match self {
            Self::LaunchAgent => launch_agent::render(app_id, exe),
            Self::XdgAutostart => desktop::render(app_id, app_name, exe),
        }
    }

    fn is_active(self, contents: &str) -> bool {
        match self {
            Self::LaunchAgent => launch_agent::is_active(contents),
            Self::XdgAutostart => desktop::is_active(contents),
        }
    }
}

/// `~/Library/LaunchAgents`.
fn launch_agents_dir(home: Option<PathBuf>) -> Option<PathBuf> {
    home.filter(|home| home.is_absolute())
        .map(|home| home.join("Library").join("LaunchAgents"))
}

/// `$XDG_CONFIG_HOME/autostart`, falling back to `~/.config/autostart`
/// when the variable is unset, empty or relative (the XDG Base Directory
/// spec says to ignore relative paths).
fn xdg_autostart_dir(xdg_config_home: Option<OsString>, home: Option<PathBuf>) -> Option<PathBuf> {
    let config = xdg_config_home
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| {
            home.filter(|home| home.is_absolute())
                .map(|home| home.join(".config"))
        })?;
    Some(config.join("autostart"))
}

/// [`set_enabled`] with an explicit directory.
fn set_enabled_in(
    format: Format,
    dir: &Path,
    enabled: bool,
    app_id: &str,
    app_name: &str,
    exe: &Path,
) -> Result<(), PlatformError> {
    validate_app_id(app_id)?;
    let path = dir.join(format.file_name(app_id));
    if !enabled {
        return match fs::remove_file(&path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "launch at login disabled");
                Ok(())
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PlatformError::io("remove", path, error)),
        };
    }

    let exe = validate_exe(exe)?;
    validate_text("application name", app_name)?;
    let app_name = if app_name.trim().is_empty() {
        app_id
    } else {
        app_name
    };
    let contents = format.render(app_id, app_name, exe);
    if fs::read(&path).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(());
    }
    write_atomically(dir, &path, contents.as_bytes())?;
    tracing::info!(path = %path.display(), "launch at login enabled");
    Ok(())
}

/// [`is_enabled`] with an explicit directory.
fn is_enabled_in(format: Format, dir: &Path, app_id: &str) -> bool {
    if validate_app_id(app_id).is_err() {
        return false;
    }
    fs::read_to_string(dir.join(format.file_name(app_id)))
        .is_ok_and(|contents| format.is_active(&contents))
}

/// Writes `path` via a temporary file in `dir` and a rename, so readers
/// never see a half-written entry. The file is readable by everyone and
/// writable by the user only (launchd rejects group- or world-writable
/// agents).
fn write_atomically(dir: &Path, path: &Path, contents: &[u8]) -> Result<(), PlatformError> {
    fs::create_dir_all(dir).map_err(|error| PlatformError::io("create", dir, error))?;
    // The ".tmp" suffix keeps a leftover from matching "*.desktop".
    let mut file = tempfile::Builder::new()
        .prefix(".")
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|error| PlatformError::io("create a temporary file in", dir, error))?;
    let temp_path = file.path().to_owned();
    file.write_all(contents)
        .and_then(|()| set_readable(file.as_file()))
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| PlatformError::io("write", &temp_path, error))?;
    file.persist(path)
        .map_err(|error| PlatformError::io("write", path, error.error))?;
    Ok(())
}

#[cfg(unix)]
fn set_readable(file: &fs::File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    file.set_permissions(fs::Permissions::from_mode(0o644))
}

#[cfg(not(unix))]
fn set_readable(_file: &fs::File) -> io::Result<()> {
    Ok(())
}

/// A reverse-DNS id: ASCII letters, digits, `.`, `-` and `_`, not starting
/// with a dot or a dash. It becomes a file name, so no path separators.
fn validate_app_id(app_id: &str) -> Result<(), PlatformError> {
    let reason = if app_id.is_empty() {
        "is empty"
    } else if app_id.len() > 200 {
        "is longer than 200 bytes"
    } else if app_id.starts_with(['.', '-']) {
        "starts with a dot or a dash"
    } else if !app_id
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        "may only contain ASCII letters, digits, '.', '-' and '_'"
    } else {
        return Ok(());
    };
    Err(PlatformError::invalid(
        "application id",
        format!("{app_id:?} {reason}"),
    ))
}

/// The executable as UTF-8 text: absolute, and representable in both a
/// property list and a desktop entry.
fn validate_exe(exe: &Path) -> Result<&str, PlatformError> {
    let text = exe.to_str().ok_or_else(|| {
        PlatformError::invalid(
            "executable path",
            format!("{} is not valid UTF-8", exe.display()),
        )
    })?;
    if !exe.is_absolute() {
        return Err(PlatformError::invalid(
            "executable path",
            format!("{text:?} is not absolute"),
        ));
    }
    validate_text("executable path", text)?;
    Ok(text)
}

/// Rejects characters neither format can carry: control characters other
/// than tab, newline and carriage return (desktop entries escape those,
/// XML allows them), and the XML non-characters U+FFFE and U+FFFF.
fn validate_text(what: &'static str, text: &str) -> Result<(), PlatformError> {
    match text.chars().find(|&c| {
        (c.is_control() && !matches!(c, '\t' | '\n' | '\r')) || matches!(c, '\u{fffe}' | '\u{ffff}')
    }) {
        Some(c) => Err(PlatformError::invalid(
            what,
            format!("{text:?} contains the character {c:?}"),
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APP_ID: &str = "io.github.alexykn.icygui";

    fn exe() -> PathBuf {
        PathBuf::from("/opt/icygui/bin/icygui")
    }

    fn entry(dir: &Path, format: Format) -> PathBuf {
        dir.join(format.file_name(APP_ID))
    }

    #[test]
    fn round_trip_in_both_formats() {
        for format in [Format::LaunchAgent, Format::XdgAutostart] {
            let temp = tempfile::tempdir().unwrap();
            let dir = temp.path().join("nested").join("autostart");
            assert!(!is_enabled_in(format, &dir, APP_ID));

            set_enabled_in(format, &dir, true, APP_ID, "icygui", &exe()).unwrap();
            assert!(is_enabled_in(format, &dir, APP_ID), "{format:?}");
            let contents = fs::read_to_string(entry(&dir, format)).unwrap();
            assert_eq!(
                contents,
                format.render(APP_ID, "icygui", "/opt/icygui/bin/icygui")
            );

            set_enabled_in(format, &dir, false, APP_ID, "icygui", &exe()).unwrap();
            assert!(!is_enabled_in(format, &dir, APP_ID));
            assert!(!entry(&dir, format).exists());

            // Disabling twice is fine; only our file is removed.
            set_enabled_in(format, &dir, false, APP_ID, "icygui", &exe()).unwrap();
            assert_eq!(fs::read_dir(&dir).unwrap().count(), 0, "no temp files left");
        }
    }

    #[test]
    fn file_names_follow_the_app_id() {
        assert_eq!(
            Format::LaunchAgent.file_name(APP_ID),
            "io.github.alexykn.icygui.plist"
        );
        assert_eq!(
            Format::XdgAutostart.file_name(APP_ID),
            "io.github.alexykn.icygui.desktop"
        );
    }

    #[test]
    fn enabling_again_updates_a_moved_executable_only() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let format = Format::XdgAutostart;
        set_enabled_in(format, dir, true, APP_ID, "icygui", &exe()).unwrap();
        let path = entry(dir, format);

        // Same contents: the file is left alone (no rewrite, no new inode).
        let before = fs::metadata(&path).unwrap();
        set_enabled_in(format, dir, true, APP_ID, "icygui", &exe()).unwrap();
        let after = fs::metadata(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(before.ino(), after.ino());
        }
        assert_eq!(before.modified().unwrap(), after.modified().unwrap());

        let moved = Path::new("/home/me/Applications/icygui");
        set_enabled_in(format, dir, true, APP_ID, "icygui", moved).unwrap();
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("Exec=/home/me/Applications/icygui --background\n"));
        assert_eq!(fs::read_dir(dir).unwrap().count(), 1, "no temp files left");
    }

    #[cfg(unix)]
    #[test]
    fn entries_are_not_writable_by_others() {
        use std::os::unix::fs::PermissionsExt as _;
        let temp = tempfile::tempdir().unwrap();
        for format in [Format::LaunchAgent, Format::XdgAutostart] {
            set_enabled_in(format, temp.path(), true, APP_ID, "icygui", &exe()).unwrap();
            let mode = fs::metadata(entry(temp.path(), format))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o644, "{format:?}");
        }
    }

    #[test]
    fn an_entry_disabled_by_the_desktop_counts_as_off() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let format = Format::XdgAutostart;
        set_enabled_in(format, dir, true, APP_ID, "icygui", &exe()).unwrap();
        let path = entry(dir, format);
        let contents = fs::read_to_string(&path).unwrap();

        fs::write(
            &path,
            contents.replace(
                "X-GNOME-Autostart-enabled=true",
                "X-GNOME-Autostart-enabled=false",
            ),
        )
        .unwrap();
        assert!(!is_enabled_in(format, dir, APP_ID));
        fs::write(&path, format!("{contents}Hidden=true\n")).unwrap();
        assert!(!is_enabled_in(format, dir, APP_ID));

        // Enabling again restores a working entry.
        set_enabled_in(format, dir, true, APP_ID, "icygui", &exe()).unwrap();
        assert!(is_enabled_in(format, dir, APP_ID));
    }

    #[test]
    fn a_disabled_launch_agent_counts_as_off() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let format = Format::LaunchAgent;
        set_enabled_in(format, dir, true, APP_ID, "icygui", &exe()).unwrap();
        let path = entry(dir, format);
        let contents = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            contents.replace("<dict>\n", "<dict>\n\t<key>Disabled</key>\n\t<true/>\n"),
        )
        .unwrap();
        assert!(!is_enabled_in(format, dir, APP_ID));
    }

    #[test]
    fn unreadable_entries_count_as_off() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let format = Format::XdgAutostart;
        fs::write(entry(dir, format), b"[Desktop Entry]\nName=\xff\n").unwrap();
        assert!(!is_enabled_in(format, dir, APP_ID));
        fs::remove_file(entry(dir, format)).unwrap();
        fs::create_dir(entry(dir, format)).unwrap();
        assert!(!is_enabled_in(format, dir, APP_ID));
        assert!(!is_enabled_in(format, dir, "../escape"));
    }

    #[test]
    fn invalid_arguments_are_rejected_before_touching_files() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let format = Format::XdgAutostart;
        for app_id in [
            "",
            "../evil",
            "a/b",
            ".hidden",
            "-flag",
            "spaced id",
            "ümlaut",
        ] {
            let error = set_enabled_in(format, dir, true, app_id, "x", &exe()).unwrap_err();
            assert!(
                matches!(
                    error,
                    PlatformError::InvalidArgument {
                        what: "application id",
                        ..
                    }
                ),
                "{app_id:?}: {error}"
            );
            assert!(set_enabled_in(format, dir, false, app_id, "x", &exe()).is_err());
        }
        for exe in [
            "icygui",
            "bin/icygui",
            "/opt/ic\u{7}ygui",
            "/opt/ic\u{0}ygui",
        ] {
            let error =
                set_enabled_in(format, dir, true, APP_ID, "icygui", Path::new(exe)).unwrap_err();
            assert!(
                matches!(
                    error,
                    PlatformError::InvalidArgument {
                        what: "executable path",
                        ..
                    }
                ),
                "{exe:?}: {error}"
            );
        }
        let error =
            set_enabled_in(format, dir, true, APP_ID, "ic\u{1b}[31mgui", &exe()).unwrap_err();
        assert!(matches!(
            error,
            PlatformError::InvalidArgument {
                what: "application name",
                ..
            }
        ));
        assert_eq!(fs::read_dir(dir).unwrap().count(), 0);

        // Disabling never needs a valid executable or name.
        set_enabled_in(format, dir, false, APP_ID, "", Path::new("relative")).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_executables_are_rejected() {
        use std::os::unix::ffi::OsStrExt as _;
        let temp = tempfile::tempdir().unwrap();
        let exe = Path::new(std::ffi::OsStr::from_bytes(b"/opt/\xffbin/icygui"));
        let error = set_enabled_in(
            Format::XdgAutostart,
            temp.path(),
            true,
            APP_ID,
            "icygui",
            exe,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not valid UTF-8"), "{error}");
    }

    #[test]
    fn an_empty_name_falls_back_to_the_app_id() {
        let temp = tempfile::tempdir().unwrap();
        set_enabled_in(
            Format::XdgAutostart,
            temp.path(),
            true,
            APP_ID,
            "  ",
            &exe(),
        )
        .unwrap();
        let contents = fs::read_to_string(entry(temp.path(), Format::XdgAutostart)).unwrap();
        assert!(
            contents.contains("\nName=io.github.alexykn.icygui\n"),
            "{contents}"
        );
    }

    #[test]
    fn write_failures_are_reported_with_the_path() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("not-a-directory");
        fs::write(&blocker, "").unwrap();
        let error = set_enabled_in(
            Format::XdgAutostart,
            &blocker,
            true,
            APP_ID,
            "icygui",
            &exe(),
        )
        .unwrap_err();
        assert!(matches!(error, PlatformError::Io { .. }), "{error}");
        assert!(error.to_string().contains("not-a-directory"), "{error}");
    }

    #[test]
    fn xdg_directory_resolution() {
        let home = Some(PathBuf::from("/home/me"));
        assert_eq!(
            xdg_autostart_dir(Some("/custom/config".into()), home.clone()),
            Some(PathBuf::from("/custom/config/autostart"))
        );
        for ignored in ["", "relative/config"] {
            assert_eq!(
                xdg_autostart_dir(Some(ignored.into()), home.clone()),
                Some(PathBuf::from("/home/me/.config/autostart")),
                "{ignored:?}"
            );
        }
        assert_eq!(
            xdg_autostart_dir(None, home),
            Some(PathBuf::from("/home/me/.config/autostart"))
        );
        assert_eq!(xdg_autostart_dir(None, None), None);
        assert_eq!(xdg_autostart_dir(None, Some(PathBuf::new())), None);
        assert_eq!(
            xdg_autostart_dir(Some("/x".into()), None),
            Some(PathBuf::from("/x/autostart"))
        );
    }

    #[test]
    fn launch_agents_directory_resolution() {
        assert_eq!(
            launch_agents_dir(Some(PathBuf::from("/Users/me"))),
            Some(PathBuf::from("/Users/me/Library/LaunchAgents"))
        );
        assert_eq!(launch_agents_dir(Some(PathBuf::from("relative"))), None);
        assert_eq!(launch_agents_dir(None), None);
    }

    #[test]
    fn the_platform_format_matches_the_target() {
        let expected = if cfg!(target_os = "macos") {
            Some(Format::LaunchAgent)
        } else if cfg!(unix) {
            Some(Format::XdgAutostart)
        } else {
            None
        };
        assert_eq!(Format::current(), expected);
    }
}
