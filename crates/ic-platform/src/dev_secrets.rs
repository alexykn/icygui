//! [`DirSecrets`]: passwords as plain files in a directory, for development
//! and headless runs only (screenshots under Xvfb, the demo harness), where
//! no keychain or Secret Service runs.
//!
//! Never a default: the app uses it only when [`DEV_SECRETS_ENV`] names a
//! directory, and logs a warning at start when it does. Each environment's
//! password is the file `<dir>/<environment id>`, its whole content (one
//! trailing line break is ignored), written user-only (`0600`, in a `0700`
//! directory on Unix). Anyone who can read the files can read the
//! passwords: it is meant for disposable test installations such as the
//! demo cluster (docs/demo.md), never for real credentials.

use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use ic_core::ports::{SecretError, SecretStore};
use secrecy::{ExposeSecret as _, SecretString};

/// The environment variable that switches the app to [`DirSecrets`]: the
/// directory holding the password files.
pub const DEV_SECRETS_ENV: &str = "ICYGUI_DEV_SECRETS_DIR";

/// [`SecretStore`] in plain files, one per environment id (see the module
/// docs). For development and headless runs only.
#[derive(Debug, Clone)]
pub struct DirSecrets {
    dir: PathBuf,
}

impl DirSecrets {
    /// Passwords in `dir` (created on the first write).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store [`DEV_SECRETS_ENV`] names, if it is set to a non-empty
    /// path.
    pub fn from_env() -> Option<Self> {
        std::env::var_os(DEV_SECRETS_ENV)
            .filter(|dir| !dir.is_empty())
            .map(Self::new)
    }

    /// The directory holding the password files.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file of `account`. Environment ids are UUIDs; anything that could
    /// leave the directory is refused.
    fn file(&self, account: &str) -> Result<PathBuf, SecretError> {
        let plain = !account.is_empty()
            && account != "."
            && account != ".."
            && account
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if plain {
            Ok(self.dir.join(account))
        } else {
            Err(SecretError::new(
                "the account (environment id) is not a plain file name",
            ))
        }
    }

    fn failed(&self, action: &str, error: &io::Error) -> SecretError {
        SecretError::new(format!(
            "cannot {action} a password in {}: {error}",
            self.dir.display()
        ))
    }
}

impl SecretStore for DirSecrets {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError> {
        let file = self.file(account)?;
        match fs::read_to_string(&file) {
            Ok(text) => {
                let text = text.strip_suffix('\n').map_or(text.as_str(), |line| {
                    line.strip_suffix('\r').unwrap_or(line)
                });
                Ok((!text.is_empty()).then(|| SecretString::from(text.to_owned())))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(self.failed("read", &error)),
        }
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretError> {
        let file = self.file(account)?;
        create_private_dir(&self.dir).map_err(|error| self.failed("store", &error))?;
        let staged = self.dir.join(format!(".{account}.tmp"));
        write_private(&staged, secret.expose_secret().as_bytes())
            .and_then(|()| fs::rename(&staged, &file))
            .map_err(|error| {
                let _ = fs::remove_file(&staged);
                self.failed("store", &error)
            })
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        let file = self.file(account)?;
        match fs::remove_file(&file) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(self.failed("delete", &error)),
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)
}

fn write_private(file: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut handle = options.open(file)?;
    handle.write_all(bytes)?;
    handle.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(text: &str) -> SecretString {
        SecretString::from(text.to_owned())
    }

    #[test]
    fn stores_reads_and_deletes_a_password_per_environment() {
        let dir = tempfile::tempdir().unwrap();
        let store = DirSecrets::new(dir.path().join("secrets"));
        assert!(store.get("env-1").unwrap().is_none());
        store.set("env-1", &secret("demo password")).unwrap();
        store.set("env-2", &secret("other")).unwrap();
        assert_eq!(
            store.get("env-1").unwrap().unwrap().expose_secret(),
            "demo password"
        );
        store.set("env-1", &secret("changed")).unwrap();
        assert_eq!(
            store.get("env-1").unwrap().unwrap().expose_secret(),
            "changed"
        );
        store.delete("env-1").unwrap();
        store.delete("env-1").unwrap();
        assert!(store.get("env-1").unwrap().is_none());
        assert_eq!(
            store.get("env-2").unwrap().unwrap().expose_secret(),
            "other"
        );
    }

    #[test]
    fn a_file_written_by_hand_may_end_with_a_line_break() {
        let dir = tempfile::tempdir().unwrap();
        let store = DirSecrets::new(dir.path());
        fs::write(dir.path().join("env"), "icygui-demo-password\n").unwrap();
        assert_eq!(
            store.get("env").unwrap().unwrap().expose_secret(),
            "icygui-demo-password"
        );
        fs::write(dir.path().join("env"), "\n").unwrap();
        assert!(store.get("env").unwrap().is_none(), "empty is no password");
    }

    #[test]
    fn accounts_that_could_leave_the_directory_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = DirSecrets::new(dir.path());
        for account in ["", ".", "..", "../x", "a/b", "a\\b"] {
            assert!(store.get(account).is_err(), "{account:?}");
            assert!(store.set(account, &secret("x")).is_err(), "{account:?}");
            assert!(store.delete(account).is_err(), "{account:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_are_user_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let store = DirSecrets::new(dir.path().join("secrets"));
        store.set("env", &secret("x")).unwrap();
        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir.path().join("secrets")), 0o700);
        assert_eq!(mode(&dir.path().join("secrets/env")), 0o600);
    }

    #[test]
    fn errors_never_quote_the_password() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("file");
        fs::write(&blocker, "").unwrap();
        // The directory is a file: storing fails.
        let store = DirSecrets::new(&blocker);
        let error = store.set("env", &secret("very secret")).unwrap_err();
        assert!(!format!("{error:?}").contains("very secret"), "{error:?}");
    }
}
