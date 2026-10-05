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
        let log_dir = system_log_dir(&project).ok_or(ConfigError::NoHomeDirectory)?;
        Ok(Self {
            config_file: project.config_dir().join(CONFIG_FILE),
            data_dir: project.data_dir().to_path_buf(),
            log_dir,
        })
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

/// Logs go where each platform's tools look for them: `~/Library/Logs` on
/// macOS (Console.app), the XDG state directory elsewhere.
fn system_log_dir(project: &ProjectDirs) -> Option<PathBuf> {
    if cfg!(target_os = "macos") {
        let base = BaseDirs::new()?;
        Some(
            base.home_dir()
                .join("Library")
                .join("Logs")
                .join(project.project_path()),
        )
    } else {
        Some(
            project
                .state_dir()
                .unwrap_or_else(|| project.data_local_dir())
                .join("logs"),
        )
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
