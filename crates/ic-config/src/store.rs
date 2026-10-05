//! The settings file: loading with migrations, atomic saving with a backup.

use std::borrow::Cow;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::ConfigError;
use crate::files::{Step, read_text, write_atomic};
use crate::migrate::parse_config;
use crate::model::{CONFIG_VERSION, Config};

/// Written at the top of every saved settings file.
const HEADER: &str = "\
# icygui settings. Passwords are kept in the system keychain, not in this file.
# icygui rewrites this file when settings change; comments are not preserved.

";

/// Loads and saves the settings file.
///
/// Saving is atomic: the new settings go to a temporary file that replaces
/// the old one only once it is complete and on disk, so a crash or a full
/// disk never leaves a half-written file. The previous version is kept next
/// to it as `<name>.bak` (see [`ConfigStore::backup_path`]). On Unix both
/// files are readable by the user only (`0600`).
///
/// A file that can't be read is reported, never replaced: the app can
/// offer [`ConfigStore::load_backup`] or start from defaults (saving then
/// moves the unreadable file to the backup).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    /// A store for the settings file at `path` (usually
    /// [`Paths::config_file`](crate::Paths::config_file)). Nothing is read
    /// until [`ConfigStore::load`].
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// The settings file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the previous version of the settings is kept: the settings
    /// file's name with `.bak` appended, in the same directory.
    pub fn backup_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map_or_else(|| OsString::from("config.toml"), OsString::from);
        name.push(".bak");
        self.path.with_file_name(name)
    }

    /// Reads the settings, upgrading older formats (see [`migrate`]).
    /// A missing file gives [`Config::default`].
    ///
    /// Keys this version doesn't know are ignored and logged. Entries
    /// without a unique id (common in hand-written files) get fresh ones
    /// ([`Config::repair_ids`]), and the file is saved right away so the ids
    /// stay stable, with the original kept as the backup. If that save
    /// fails, the settings are still returned and the failure is logged.
    ///
    /// [`migrate`]: fn@crate::migrate
    ///
    /// # Errors
    ///
    /// - [`ConfigError::Io`] when the file exists but can't be read;
    /// - [`ConfigError::Parse`] when it is not valid UTF-8 or TOML, or its
    ///   content doesn't fit the settings (the message has the line);
    /// - [`ConfigError::InvalidVersion`] or
    ///   [`ConfigError::UnsupportedVersion`] when its `version` is unusable
    ///   or newer than this build.
    pub fn load(&self) -> Result<Config, ConfigError> {
        let Some(text) = read_text(&self.path)? else {
            return Ok(Config::default());
        };
        let mut config = parse_config(&text)?;
        let repaired = config.repair_ids();
        if repaired > 0 {
            tracing::warn!(
                count = repaired,
                path = %self.path.display(),
                "gave new ids to settings entries without a unique id"
            );
            if let Err(error) = self.save(&config) {
                tracing::warn!(%error, "could not save the new ids; they change on the next start");
            }
        }
        Ok(config)
    }

    /// Reads the backup copy, the settings as they were before the last
    /// save, for restoring them after [`ConfigStore::load`] failed. To
    /// restore it, [`ConfigStore::save`] the result. `None` if there is no
    /// backup.
    ///
    /// Like [`ConfigStore::load`] it upgrades older formats and repairs ids,
    /// but it never writes anything.
    ///
    /// # Errors
    ///
    /// As for [`ConfigStore::load`], for the backup file.
    pub fn load_backup(&self) -> Result<Option<Config>, ConfigError> {
        let Some(text) = read_text(&self.backup_path())? else {
            return Ok(None);
        };
        let mut config = parse_config(&text)?;
        config.repair_ids();
        Ok(Some(config))
    }

    /// Writes the settings atomically, in the current format, keeping the
    /// previous file as the backup. Missing directories are created
    /// (user-only on Unix). If the file is a symbolic link, the file it
    /// points to is replaced and the link kept. Saving unchanged settings
    /// writes nothing.
    ///
    /// # Errors
    ///
    /// - [`ConfigError::Serialize`] when the settings can't be written as
    ///   TOML (a path that isn't valid UTF-8); nothing is written;
    /// - [`ConfigError::Io`] when a directory, the temporary file, the
    ///   backup or the final rename fails. The settings file then still has
    ///   its previous contents.
    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        self.save_with(config, |_| Ok(()))
    }

    /// [`ConfigStore::save`] with a hook after each step of the write, for
    /// simulating crashes in tests.
    fn save_with(
        &self,
        config: &Config,
        checkpoint: impl FnMut(Step) -> io::Result<()>,
    ) -> Result<(), ConfigError> {
        let text = to_toml(config)?;
        write_atomic(&self.path, &self.backup_path(), text.as_bytes(), checkpoint)
    }
}

/// The settings as TOML, stamped with the current format version.
fn to_toml(config: &Config) -> Result<String, ConfigError> {
    let config = if config.version == CONFIG_VERSION {
        Cow::Borrowed(config)
    } else {
        Cow::Owned(Config {
            version: CONFIG_VERSION,
            ..config.clone()
        })
    };
    let body = toml::to_string(config.as_ref())
        .map_err(|error| ConfigError::Serialize(error.to_string()))?;
    Ok(format!("{HEADER}{body}"))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::model::{AuthConfig, Environment};

    fn store(dir: &Path) -> ConfigStore {
        ConfigStore::new(dir.join("config.toml"))
    }

    fn config_named(name: &str) -> Config {
        Config {
            environments: vec![Environment::new(
                name,
                "https://master-01:5665",
                AuthConfig::Basic {
                    username: "icygui".to_owned(),
                },
            )],
            ..Config::default()
        }
    }

    #[test]
    fn backup_path_appends_bak() {
        let store = ConfigStore::new(PathBuf::from("/home/u/.config/icygui/config.toml"));
        assert_eq!(
            store.backup_path(),
            Path::new("/home/u/.config/icygui/config.toml.bak")
        );
        assert_eq!(
            store.path(),
            Path::new("/home/u/.config/icygui/config.toml")
        );
        assert_eq!(
            ConfigStore::new(PathBuf::from("settings")).backup_path(),
            Path::new("settings.bak")
        );
    }

    #[test]
    fn saved_files_start_with_the_header_and_current_version() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let mut config = config_named("prod");
        config.version = 0;
        store.save(&config).unwrap();
        let text = fs::read_to_string(store.path()).unwrap();
        assert!(text.starts_with(HEADER));
        assert!(
            text.contains(&format!("\nversion = {CONFIG_VERSION}\n")),
            "{text}"
        );
        let loaded = store.load().unwrap();
        assert_eq!(loaded.version, CONFIG_VERSION);
        assert_eq!(loaded.environments, config.environments);
    }

    #[test]
    fn interrupted_saves_keep_the_old_file_and_backup() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let first = config_named("first");
        let second = config_named("second");
        store.save(&first).unwrap();
        store.save(&second).unwrap();
        let file_before = fs::read(store.path()).unwrap();
        let backup_before = fs::read(store.backup_path()).unwrap();

        // A crash right after the temporary file is written: nothing changed.
        let result = store.save_with(&config_named("third"), |step| match step {
            Step::TempWritten => Err(io::Error::other("simulated crash")),
            Step::BackedUp => Ok(()),
        });
        assert!(result.is_err());
        assert_eq!(fs::read(store.path()).unwrap(), file_before);
        assert_eq!(fs::read(store.backup_path()).unwrap(), backup_before);
        assert_eq!(store.load().unwrap(), second);
        assert_eq!(store.load_backup().unwrap(), Some(first));

        // A crash just before the rename: the file is intact, and the
        // backup is a copy of it.
        let result = store.save_with(&config_named("third"), |step| match step {
            Step::TempWritten => Ok(()),
            Step::BackedUp => Err(io::Error::other("simulated crash")),
        });
        assert!(result.is_err());
        assert_eq!(fs::read(store.path()).unwrap(), file_before);
        assert_eq!(store.load().unwrap(), second);
        assert_eq!(store.load_backup().unwrap(), Some(second));

        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["config.toml", "config.toml.bak"],
            "no temporary files left"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unencodable_settings_write_nothing() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt as _;

        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let good = config_named("prod");
        store.save(&good).unwrap();
        let mut bad = good.clone();
        bad.environments[0].auth = AuthConfig::ClientCertificate {
            cert_path: PathBuf::from(OsStr::from_bytes(b"/certs/\xff.pem")),
            key_path: PathBuf::from("/certs/client.key"),
        };
        assert!(matches!(store.save(&bad), Err(ConfigError::Serialize(_))));
        assert_eq!(store.load().unwrap(), good);
        assert!(!store.backup_path().exists());
    }
}
