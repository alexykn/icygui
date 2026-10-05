//! [`KeyringSecrets`]: environment passwords in the OS credential store.
//!
//! Built on the keyring 4.x ecosystem: `keyring-core` for the API, the
//! macOS login keychain (`apple-native-keyring-store`) and the freedesktop
//! Secret Service over D-Bus (`zbus-secret-service-keyring-store`, pure-Rust
//! crypto) for storage. These are the stores `keyring` 4's own default
//! picks; depending on them directly lets the adapter connect lazily, retry
//! after a failed connection and run its tests against the in-memory mock
//! store.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError};

use ic_core::ports::{SecretError, SecretStore};
use keyring_core::{CredentialStore, Entry, Error as KeyringError};
use secrecy::zeroize::Zeroize as _;
use secrecy::{ExposeSecret as _, SecretString};

/// The service name every icygui secret is stored under. The account is the
/// environment id.
pub const SERVICE: &str = "io.github.alexykn.icygui";

/// Opens a credential store; called again after a connection failure.
type Connect = dyn Fn() -> keyring_core::Result<Arc<CredentialStore>> + Send + Sync;

/// [`SecretStore`] backed by the OS credential store: the login keychain on
/// macOS, the Secret Service (GNOME Keyring, `KWallet`, `KeePassXC`, …) on Linux.
///
/// Secrets are stored under the service [`SERVICE`] with the environment id
/// as the account.
///
/// The store is opened on first use, not in [`KeyringSecrets::new`], and
/// opened again after a platform failure, so a keyring daemon that starts
/// after the app (common right after login) or restarts is picked up
/// without restarting the app.
///
/// # Blocking
///
/// Every call blocks: the OS may ask the user to unlock the keyring or to
/// allow access, and the call waits for the answer. Call it from a thread
/// that may block (for example `tokio::task::spawn_blocking`), never from
/// the UI thread. Calls are serialised, so concurrent writers can't create
/// duplicate items.
pub struct KeyringSecrets {
    service: String,
    store_name: &'static str,
    connect: Box<Connect>,
    /// The open store. The lock is held for a whole operation.
    store: Mutex<Option<Arc<CredentialStore>>>,
}

impl KeyringSecrets {
    /// Secrets in the OS credential store.
    pub fn new() -> Self {
        Self::with_connect(platform_store_name(), Box::new(platform_store))
    }

    /// Secrets in `store`, for example `keyring_core::mock::Store` in tests
    /// or another keyring-compatible store.
    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self::with_connect("credential store", Box::new(move || Ok(Arc::clone(&store))))
    }

    fn with_connect(store_name: &'static str, connect: Box<Connect>) -> Self {
        Self {
            service: SERVICE.to_owned(),
            store_name,
            connect,
            store: Mutex::new(None),
        }
    }

    /// The service name secrets are stored under ([`SERVICE`]).
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Runs `operation` on the entry for `account`, holding the store lock.
    fn with_entry<T>(
        &self,
        verb: Verb,
        account: &str,
        operation: impl FnOnce(&Entry) -> Result<T, KeyringError>,
    ) -> Result<T, SecretError> {
        if account.is_empty() {
            return Err(SecretError::new("the account (environment id) is empty"));
        }
        let mut slot = self.lock();
        let store = if let Some(store) = slot.as_ref() {
            Arc::clone(store)
        } else {
            let store = (self.connect)().map_err(|error| self.open_failed(&error))?;
            *slot = Some(Arc::clone(&store));
            store
        };
        let outcome = store
            .build(&self.service, account, None)
            .and_then(|entry| operation(&entry));
        outcome.map_err(|error| {
            if matches!(error, KeyringError::PlatformFailure(_)) {
                // Possibly a dead D-Bus connection or a restarted daemon:
                // open the store again next time.
                *slot = None;
            }
            self.failed(verb, account, error)
        })
    }

    fn lock(&self) -> MutexGuard<'_, Option<Arc<CredentialStore>>> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn open_failed(&self, error: &KeyringError) -> SecretError {
        let reason = describe(error);
        tracing::debug!(store = self.store_name, %reason, "cannot open the credential store");
        SecretError::new(format!("cannot open the {}: {reason}", self.store_name))
    }

    fn failed(&self, verb: Verb, account: &str, error: KeyringError) -> SecretError {
        let reason = describe(&error);
        // The error may hold secret bytes (bad encodings) or entries; wipe
        // and drop it here, it never leaves this function.
        wipe(error);
        tracing::debug!(
            store = self.store_name,
            account,
            operation = verb.as_str(),
            %reason,
            "credential store request failed"
        );
        SecretError::new(format!(
            "cannot {} the password {} the {}: {reason}",
            verb.as_str(),
            verb.preposition(),
            self.store_name
        ))
    }
}

impl Default for KeyringSecrets {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for KeyringSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never wait for the lock: an operation may be waiting for the user
        // to answer an unlock prompt.
        let state = match self.store.try_lock() {
            Ok(slot) => open_state(slot.as_ref()),
            Err(TryLockError::Poisoned(poisoned)) => open_state(poisoned.into_inner().as_ref()),
            Err(TryLockError::WouldBlock) => "busy",
        };
        f.debug_struct("KeyringSecrets")
            .field("service", &self.service)
            .field("store", &self.store_name)
            .field("state", &state)
            .finish_non_exhaustive()
    }
}

fn open_state(store: Option<&Arc<CredentialStore>>) -> &'static str {
    if store.is_some() { "open" } else { "closed" }
}

impl SecretStore for KeyringSecrets {
    fn get(&self, account: &str) -> Result<Option<SecretString>, SecretError> {
        self.with_entry(Verb::Read, account, |entry| match entry.get_secret() {
            Ok(bytes) => decode(bytes).map(Some),
            Err(KeyringError::NoEntry) => Ok(None),
            Err(error) => Err(error),
        })
    }

    fn set(&self, account: &str, secret: &SecretString) -> Result<(), SecretError> {
        let bytes = secret.expose_secret().as_bytes();
        self.with_entry(Verb::Write, account, |entry| {
            match entry.set_secret(bytes) {
                Err(KeyringError::Ambiguous(duplicates)) => {
                    // Several items match (written by another tool, or an old
                    // race): replace them all with a single one.
                    delete_all(duplicates)?;
                    entry.set_secret(bytes)
                }
                result => result,
            }
        })
    }

    fn delete(&self, account: &str) -> Result<(), SecretError> {
        self.with_entry(Verb::Delete, account, |entry| {
            match entry.delete_credential() {
                Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
                Err(KeyringError::Ambiguous(duplicates)) => delete_all(duplicates),
                Err(error) => Err(error),
            }
        })
    }
}

#[derive(Clone, Copy, Debug)]
enum Verb {
    Read,
    Write,
    Delete,
}

impl Verb {
    fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "save",
            Self::Delete => "delete",
        }
    }

    fn preposition(self) -> &'static str {
        match self {
            Self::Read | Self::Delete => "from",
            Self::Write => "in",
        }
    }
}

/// Deletes every entry; entries that are already gone are fine.
fn delete_all(entries: Vec<Entry>) -> Result<(), KeyringError> {
    for entry in entries {
        match entry.delete_credential() {
            Ok(()) | Err(KeyringError::NoEntry) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Turns the stored bytes into a secret string, wiping every intermediate
/// copy. The result is allocated at its exact size, so converting it never
/// reallocates and leaves a copy behind.
fn decode(mut bytes: Vec<u8>) -> Result<SecretString, KeyringError> {
    let decoded = match std::str::from_utf8(&bytes) {
        Ok(text) => Ok(SecretString::from(Box::<str>::from(text))),
        // An empty vector: the bytes are wiped below, not handed out.
        Err(_) => Err(KeyringError::BadEncoding(Vec::new())),
    };
    bytes.zeroize();
    decoded
}

/// Wipes secret bytes an error may carry before it is dropped.
fn wipe(error: KeyringError) {
    match error {
        KeyringError::BadEncoding(mut bytes) | KeyringError::BadDataFormat(mut bytes, _) => {
            bytes.zeroize();
        }
        _ => {}
    }
}

/// A user-facing reason for a keyring error. Never includes secret data:
/// the raw bytes of bad encodings and the `Debug` output of entries (which
/// for some stores contains the secret) are left out.
fn describe(error: &KeyringError) -> String {
    match error {
        KeyringError::PlatformFailure(source) => source.to_string(),
        KeyringError::NoStorageAccess(source) => {
            format!("the keyring is locked or access was denied ({source})")
        }
        KeyringError::NoEntry => "there is no such entry".to_owned(),
        KeyringError::BadEncoding(_) => "the stored password is not valid UTF-8".to_owned(),
        KeyringError::BadDataFormat(..) => "the stored password is malformed".to_owned(),
        KeyringError::BadStoreFormat(reason) => format!("the store's data is malformed ({reason})"),
        KeyringError::TooLong(what, limit) => {
            format!("the {what} is longer than the store's limit of {limit}")
        }
        KeyringError::Invalid(what, reason) => format!("invalid {what}: {reason}"),
        KeyringError::Ambiguous(entries) => format!(
            "{} entries match; remove the duplicates in your keyring manager",
            entries.len()
        ),
        KeyringError::NoDefaultStore => "no credential store is available".to_owned(),
        KeyringError::NotSupportedByStore(reason) => format!("not supported ({reason})"),
        // `Error` is non-exhaustive; keyring's `Display` keeps secrets out.
        other => other.to_string(),
    }
}

/// The name of the OS store, for messages.
fn platform_store_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS keychain"
    } else if cfg!(unix) {
        "Secret Service"
    } else {
        "credential store"
    }
}

#[cfg(target_os = "macos")]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
    Ok(store)
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
    Ok(store)
}

#[cfg(not(any(
    target_os = "macos",
    all(
        unix,
        not(any(target_os = "macos", target_os = "ios", target_os = "android"))
    )
)))]
fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    Err(KeyringError::NotSupportedByStore(
        "there is no supported credential store on this platform".to_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use keyring_core::api::CredentialStoreApi as _;
    use keyring_core::mock;

    use super::*;

    fn mock_store() -> Arc<mock::Store> {
        mock::Store::new().unwrap()
    }

    fn secrets(store: &Arc<mock::Store>) -> KeyringSecrets {
        let store: Arc<CredentialStore> = store.clone();
        KeyringSecrets::with_store(store)
    }

    /// The mock credential behind `account`, to inspect it or inject errors.
    fn cred(store: &Arc<mock::Store>, account: &str) -> Entry {
        store.build(SERVICE, account, None).unwrap()
    }

    fn inject(entry: &Entry, error: KeyringError) {
        let cred: &mock::Cred = entry.as_any().downcast_ref().unwrap();
        cred.set_error(error);
    }

    fn secret(text: &str) -> SecretString {
        SecretString::from(text)
    }

    fn platform_error(message: &str) -> Box<dyn std::error::Error + Send + Sync> {
        message.into()
    }

    #[test]
    fn missing_secret_is_none() {
        let store = mock_store();
        assert!(secrets(&store).get("env-1").unwrap().is_none());
    }

    #[test]
    fn set_get_replace_delete_round_trip() {
        let store = mock_store();
        let secrets = secrets(&store);

        secrets.set("env-1", &secret("hunter2")).unwrap();
        assert_eq!(
            secrets.get("env-1").unwrap().unwrap().expose_secret(),
            "hunter2"
        );

        secrets.set("env-1", &secret("correct horse")).unwrap();
        assert_eq!(
            secrets.get("env-1").unwrap().unwrap().expose_secret(),
            "correct horse"
        );

        secrets.delete("env-1").unwrap();
        assert!(secrets.get("env-1").unwrap().is_none());
    }

    #[test]
    fn stored_under_the_service_with_the_environment_id_as_account() {
        let store = mock_store();
        secrets(&store).set("3f2a-env", &secret("pw")).unwrap();
        assert_eq!(cred(&store, "3f2a-env").get_password().unwrap(), "pw");
        assert!(matches!(
            store
                .build("another.service", "3f2a-env", None)
                .unwrap()
                .get_password(),
            Err(KeyringError::NoEntry)
        ));
        assert_eq!(secrets(&store).service(), "io.github.alexykn.icygui");
    }

    #[test]
    fn accounts_are_independent() {
        let store = mock_store();
        let secrets = secrets(&store);
        secrets.set("a", &secret("one")).unwrap();
        secrets.set("b", &secret("two")).unwrap();
        secrets.delete("a").unwrap();
        assert!(secrets.get("a").unwrap().is_none());
        assert_eq!(secrets.get("b").unwrap().unwrap().expose_secret(), "two");
    }

    #[test]
    fn deleting_a_missing_secret_succeeds() {
        let store = mock_store();
        let secrets = secrets(&store);
        secrets.delete("never-set").unwrap();
        secrets.set("x", &secret("pw")).unwrap();
        secrets.delete("x").unwrap();
        secrets.delete("x").unwrap();
    }

    #[test]
    fn unicode_and_empty_passwords_survive() {
        let store = mock_store();
        let secrets = secrets(&store);
        for password in ["pässwörd ✓ 🔑", "", " leading and trailing "] {
            secrets.set("env", &secret(password)).unwrap();
            assert_eq!(
                secrets.get("env").unwrap().unwrap().expose_secret(),
                password
            );
        }
    }

    #[test]
    fn empty_account_is_rejected() {
        let store = mock_store();
        let secrets = secrets(&store);
        assert!(secrets.get("").is_err());
        assert!(secrets.set("", &secret("pw")).is_err());
        assert!(secrets.delete("").is_err());
    }

    #[test]
    fn locked_keyring_is_an_error_with_a_reason() {
        let store = mock_store();
        let secrets = secrets(&store);
        inject(
            &cred(&store, "env"),
            KeyringError::NoStorageAccess(platform_error("collection is locked")),
        );
        let error = secrets.get("env").unwrap_err();
        assert!(
            error.message.contains("cannot read the password"),
            "{error}"
        );
        assert!(error.message.contains("collection is locked"), "{error}");
    }

    #[test]
    fn every_operation_reports_failures() {
        let store = mock_store();
        let secrets = secrets(&store);
        let entry = cred(&store, "env");

        inject(
            &entry,
            KeyringError::PlatformFailure(platform_error("boom")),
        );
        let error = secrets.set("env", &secret("pw")).unwrap_err();
        assert!(
            error.message.starts_with("cannot save the password in"),
            "{error}"
        );
        assert!(error.message.contains("boom"), "{error}");

        secrets.set("env", &secret("pw")).unwrap();
        inject(
            &entry,
            KeyringError::PlatformFailure(platform_error("boom")),
        );
        let error = secrets.delete("env").unwrap_err();
        assert!(
            error.message.starts_with("cannot delete the password from"),
            "{error}"
        );
        assert_eq!(
            secrets.get("env").unwrap().unwrap().expose_secret(),
            "pw",
            "a failed delete keeps the secret"
        );
    }

    #[test]
    fn invalid_utf8_is_an_error_without_the_bytes() {
        let store = mock_store();
        let entry = cred(&store, "env");
        entry.set_secret(b"hunt\xffer2").unwrap();
        let error = secrets(&store).get("env").unwrap_err();
        assert!(error.message.contains("not valid UTF-8"), "{error}");
        assert!(!error.message.contains("hunt"), "{error}");
    }

    #[test]
    fn error_messages_never_contain_secrets() {
        let store = mock_store();
        let secrets = secrets(&store);
        secrets.set("env", &secret("hunter2")).unwrap();
        // The mock's `Debug` prints its stored bytes; an ambiguity error
        // wrapping such entries must not leak them.
        let duplicates = vec![cred(&store, "env"), cred(&store, "env")];
        inject(&cred(&store, "env"), KeyringError::Ambiguous(duplicates));
        let error = secrets.get("env").unwrap_err();
        let bytes = format!("{:?}", b"hunter2".as_slice());
        for text in [error.message.clone(), format!("{error:?}")] {
            assert!(!text.contains("hunter2"), "{text}");
            assert!(!text.contains(&bytes[1..bytes.len() - 1]), "{text}");
        }
        assert!(error.message.contains("2 entries match"), "{error}");

        inject(
            &cred(&store, "env"),
            KeyringError::BadDataFormat(b"hunter2".to_vec(), platform_error("hunter2")),
        );
        let error = secrets.get("env").unwrap_err();
        assert!(!error.message.contains("hunter2"), "{error}");
    }

    #[test]
    fn duplicates_are_replaced_on_save_and_removed_on_delete() {
        let store = mock_store();
        let secrets = secrets(&store);
        let first = cred(&store, "dup-1");
        let second = cred(&store, "dup-2");
        first.set_password("old").unwrap();
        second.set_password("older").unwrap();

        inject(
            &cred(&store, "env"),
            KeyringError::Ambiguous(vec![cred(&store, "dup-1"), cred(&store, "dup-2")]),
        );
        secrets.set("env", &secret("new")).unwrap();
        assert!(matches!(first.get_password(), Err(KeyringError::NoEntry)));
        assert!(matches!(second.get_password(), Err(KeyringError::NoEntry)));
        assert_eq!(secrets.get("env").unwrap().unwrap().expose_secret(), "new");

        first.set_password("again").unwrap();
        inject(
            &cred(&store, "env"),
            KeyringError::Ambiguous(vec![cred(&store, "dup-1"), cred(&store, "dup-2")]),
        );
        secrets.delete("env").unwrap();
        assert!(matches!(first.get_password(), Err(KeyringError::NoEntry)));
    }

    #[test]
    fn a_store_that_cannot_open_is_retried() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let store = mock_store();
        let secrets = {
            let attempts = Arc::clone(&attempts);
            let store = store.clone();
            KeyringSecrets::with_connect(
                "Secret Service",
                Box::new(move || {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err(KeyringError::PlatformFailure(platform_error(
                            "org.freedesktop.DBus.Error.ServiceUnknown",
                        )))
                    } else {
                        let store: Arc<CredentialStore> = store.clone();
                        Ok(store)
                    }
                }),
            )
        };

        let error = secrets.get("env").unwrap_err();
        assert!(
            error
                .message
                .starts_with("cannot open the Secret Service: "),
            "{error}"
        );
        assert!(error.message.contains("ServiceUnknown"), "{error}");

        secrets.set("env", &secret("pw")).unwrap();
        secrets.get("env").unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 2, "opened once it worked");
    }

    #[test]
    fn a_platform_failure_reopens_the_store() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let store = mock_store();
        let secrets = {
            let attempts = Arc::clone(&attempts);
            let store = store.clone();
            KeyringSecrets::with_connect(
                "Secret Service",
                Box::new(move || {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    let store: Arc<CredentialStore> = store.clone();
                    Ok(store)
                }),
            )
        };
        secrets.set("env", &secret("pw")).unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        // A locked keyring is not a connection problem: keep the store.
        inject(
            &cred(&store, "env"),
            KeyringError::NoStorageAccess(platform_error("locked")),
        );
        secrets.get("env").unwrap_err();
        secrets.get("env").unwrap();
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        inject(
            &cred(&store, "env"),
            KeyringError::PlatformFailure(platform_error("connection closed")),
        );
        secrets.get("env").unwrap_err();
        assert_eq!(secrets.get("env").unwrap().unwrap().expose_secret(), "pw");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "reopened after the failure"
        );
    }

    #[test]
    fn debug_shows_the_store_but_no_secrets() {
        let store = mock_store();
        let secrets = secrets(&store);
        secrets.set("env", &secret("hunter2")).unwrap();
        let debug = format!("{secrets:?}");
        assert!(debug.contains("io.github.alexykn.icygui"), "{debug}");
        assert!(!debug.contains("hunter2"), "{debug}");
    }

    #[test]
    fn usable_as_the_core_port() {
        let store = mock_store();
        let port: Arc<dyn SecretStore> = Arc::new(secrets(&store));
        port.set("env", &secret("pw")).unwrap();
        let handle = std::thread::spawn(move || port.get("env").unwrap().is_some());
        assert!(handle.join().unwrap());
    }

    #[test]
    fn the_platform_store_is_opened_lazily() {
        // Constructing never touches the OS store, so this is safe in CI.
        let secrets = KeyringSecrets::new();
        let debug = format!("{secrets:?}");
        assert!(debug.contains(r#"state: "closed""#), "{debug}");
        assert_eq!(secrets.service(), SERVICE);
    }
}
