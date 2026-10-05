//! Where icygui keeps its files.

use std::path::{Path, PathBuf};

use directories::{BaseDirs, ProjectDirs};

use crate::error::ConfigError;
use crate::files::create_private_dir;
use crate::store::ConfigStore;

/// Reverse-DNS qualifier, organisation and application name: the bundle
/// identifier is `io.github.alexykn.icygui`.
const QUALIFIER: &str = "io.github";
const ORGANIZATION: &str = "alexykn";
const APPLICATION: &str = "icygui";

/// The settings file's name.
const CONFIG_FILE: &str = "config.toml";

/// The locations of icygui's files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    /// The settings file, `config.toml`.
    pub config_file: PathBuf,
    /// Local data, such as the event log database.
    pub data_dir: PathBuf,
    /// Log files.
    pub log_dir: PathBuf,
}

impl Paths {
    /// The platform's standard locations:
    ///
    /// | | Linux | macOS |
    /// |---|---|---|
    /// | `config_file` | `$XDG_CONFIG_HOME/icygui/config.toml` (`~/.config/…`) | `~/Library/Application Support/io.github.alexykn.icygui/config.toml` |
    /// | `data_dir` | `$XDG_DATA_HOME/icygui` (`~/.local/share/…`) | `~/Library/Application Support/io.github.alexykn.icygui` |
    /// | `log_dir` | `$XDG_STATE_HOME/icygui/logs` (`~/.local/state/…`) | `~/Library/Logs/io.github.alexykn.icygui` |
    ///
    /// Nothing is created; see [`Paths::create_dirs`].
    ///
    /// # Errors
    ///
    /// [`ConfigError::NoHomeDirectory`] when the system reports no home
    /// directory.
    pub fn from_system() -> Result<Self, ConfigError> {
        let project = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
            .ok_or(ConfigError::NoHomeDirectory)?;
        let base = BaseDirs::new().ok_or(ConfigError::NoHomeDirectory)?;
        let platform = if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Other
        };
        Ok(layout(
            &SystemDirs {
                home: base.home_dir(),
                config: project.config_dir(),
                data: project.data_dir(),
                state: project.state_dir(),
                data_local: project.data_local_dir(),
                project: project.project_path(),
            },
            platform,
        ))
    }

    /// Everything under one directory, for tests and portable installs:
    /// `<root>/config.toml`, `<root>/data` and `<root>/logs`.
    pub fn in_dir(root: &Path) -> Self {
        Self {
            config_file: root.join(CONFIG_FILE),
            data_dir: root.join("data"),
            log_dir: root.join("logs"),
        }
    }

    /// A [`ConfigStore`] for [`Paths::config_file`].
    pub fn config_store(&self) -> ConfigStore {
        ConfigStore::new(self.config_file.clone())
    }

    /// Creates the settings, data and log directories if they are missing,
    /// readable by the user only on Unix. Existing directories are left
    /// as they are.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Io`] when a directory can't be created.
    pub fn create_dirs(&self) -> Result<(), ConfigError> {
        if let Some(config_dir) = self
            .config_file
            .parent()
            .filter(|dir| !dir.as_os_str().is_empty())
        {
            create_private_dir(config_dir)?;
        }
        create_private_dir(&self.data_dir)?;
        create_private_dir(&self.log_dir)
    }
}

/// The directories the system reports, as the `directories` crate finds
/// them (following the XDG variables on Linux).
struct SystemDirs<'a> {
    /// The user's home directory.
    home: &'a Path,
    /// icygui's settings directory.
    config: &'a Path,
    /// icygui's data directory.
    data: &'a Path,
    /// icygui's state directory, where the platform has one (Linux).
    state: Option<&'a Path>,
    /// icygui's local (non-roaming) data directory.
    data_local: &'a Path,
    /// icygui's directory name below the platform's base directories
    /// (`io.github.alexykn.icygui` on macOS, `icygui` on Linux).
    project: &'a Path,
}

/// The platforms whose layouts differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Platform {
    /// macOS: logs go to `~/Library/Logs`, where Console.app looks.
    MacOs,
    /// Linux and the rest: logs go to the XDG state directory.
    Other,
}

/// Where icygui's files go, given the system's directories.
fn layout(dirs: &SystemDirs<'_>, platform: Platform) -> Paths {
    let log_dir = match platform {
        Platform::MacOs => dirs.home.join("Library").join("Logs").join(dirs.project),
        Platform::Other => dirs.state.unwrap_or(dirs.data_local).join("logs"),
    };
    Paths {
        config_file: dirs.config.join(CONFIG_FILE),
        data_dir: dirs.data.to_path_buf(),
        log_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_dir_keeps_everything_under_the_root() {
        let root = Path::new("/opt/icygui-portable");
        let paths = Paths::in_dir(root);
        assert_eq!(paths.config_file, root.join("config.toml"));
        assert_eq!(paths.data_dir, root.join("data"));
        assert_eq!(paths.log_dir, root.join("logs"));
        assert_eq!(paths.config_store().path(), root.join("config.toml"));
        assert_eq!(
            paths.config_store().backup_path(),
            root.join("config.toml.bak")
        );
    }

    #[test]
    fn create_dirs_creates_all_three() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(&root.path().join("icygui"));
        paths.create_dirs().unwrap();
        assert!(paths.config_file.parent().unwrap().is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.log_dir.is_dir());
        assert!(
            !paths.config_file.exists(),
            "the settings file is not created"
        );
        // Running it again is fine.
        paths.create_dirs().unwrap();
    }

    #[test]
    fn create_dirs_reports_failures() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("blocker"), b"x").unwrap();
        let paths = Paths::in_dir(&root.path().join("blocker"));
        assert!(matches!(paths.create_dirs(), Err(ConfigError::Io { .. })));
    }

    #[test]
    fn create_dirs_accepts_a_bare_file_name() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            config_file: PathBuf::from("config.toml"),
            data_dir: root.path().join("data"),
            log_dir: root.path().join("logs"),
        };
        paths.create_dirs().unwrap();
        assert!(paths.data_dir.is_dir() && paths.log_dir.is_dir());
    }

    #[test]
    fn linux_follows_the_xdg_layout() {
        let paths = layout(
            &SystemDirs {
                home: Path::new("/home/m"),
                config: Path::new("/home/m/.config/icygui"),
                data: Path::new("/home/m/.local/share/icygui"),
                state: Some(Path::new("/home/m/.local/state/icygui")),
                data_local: Path::new("/home/m/.local/share/icygui"),
                project: Path::new("icygui"),
            },
            Platform::Other,
        );
        assert_eq!(
            paths,
            Paths {
                config_file: PathBuf::from("/home/m/.config/icygui/config.toml"),
                data_dir: PathBuf::from("/home/m/.local/share/icygui"),
                log_dir: PathBuf::from("/home/m/.local/state/icygui/logs"),
            }
        );
        // Without a state directory, logs go with the local data.
        let paths = layout(
            &SystemDirs {
                home: Path::new("/home/m"),
                config: Path::new("/xdg/config/icygui"),
                data: Path::new("/xdg/data/icygui"),
                state: None,
                data_local: Path::new("/xdg/data/icygui"),
                project: Path::new("icygui"),
            },
            Platform::Other,
        );
        assert_eq!(
            paths.config_file,
            Path::new("/xdg/config/icygui/config.toml")
        );
        assert_eq!(paths.log_dir, Path::new("/xdg/data/icygui/logs"));
    }

    #[test]
    fn macos_keeps_logs_where_console_looks() {
        let support = "/Users/m/Library/Application Support/io.github.alexykn.icygui";
        let paths = layout(
            &SystemDirs {
                home: Path::new("/Users/m"),
                config: Path::new(support),
                data: Path::new(support),
                state: None,
                data_local: Path::new(support),
                project: Path::new("io.github.alexykn.icygui"),
            },
            Platform::MacOs,
        );
        assert_eq!(
            paths,
            Paths {
                config_file: Path::new(support).join("config.toml"),
                data_dir: PathBuf::from(support),
                log_dir: PathBuf::from("/Users/m/Library/Logs/io.github.alexykn.icygui"),
            }
        );
    }

    #[test]
    fn system_paths_follow_platform_conventions() {
        // Only meaningful where the environment has a home directory.
        let Ok(paths) = Paths::from_system() else {
            return;
        };
        assert!(paths.config_file.is_absolute());
        assert!(paths.config_file.ends_with("config.toml"));
        if cfg!(target_os = "macos") {
            let id = "io.github.alexykn.icygui";
            assert!(
                paths
                    .config_file
                    .ends_with(format!("Library/Application Support/{id}/config.toml"))
            );
            assert!(
                paths
                    .data_dir
                    .ends_with(format!("Library/Application Support/{id}"))
            );
            assert!(paths.log_dir.ends_with(format!("Library/Logs/{id}")));
        } else if cfg!(target_os = "linux") {
            assert!(paths.config_file.ends_with("icygui/config.toml"));
            assert!(paths.data_dir.ends_with("icygui"));
            assert!(paths.log_dir.ends_with("icygui/logs"));
        }
    }
}
