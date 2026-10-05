//! [`KeyringSecrets`]: environment passwords in the OS credential store.
//!
//! Built on the keyring 4.x ecosystem: `keyring-core` for the API, the
//! macOS login keychain (`apple-native-keyring-store`) and the freedesktop
//! Secret Service over D-Bus (`zbus-secret-service-keyring-store`, pure-Rust
//! crypto) for storage. These are the stores `keyring` 4's own default
//! picks; depending on them directly lets the adapter connect lazily, retry
//! after a failed connection and run its tests against the in-memory mock
//! store.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use ic_core::ports::{SecretError, SecretStore};
use keyring_core::{CredentialStore, Entry, Error as KeyringError};
use secrecy::zeroize::Zeroize as _;
use secrecy::{ExposeSecret as _, SecretString};

/// The service name every icygui secret is stored under. The account is the
/// environment id.
pub const SERVICE: &str = "io.github.alexykn.icygui";

/// How long a call waits for an earlier call on the same account. Requests
/// take milliseconds; one that takes longer waits for the user to answer an
/// unlock or access prompt.
const BUSY_WAIT: Duration = Duration::from_secs(2);

/// Whether the platform store's connection can go stale (see
/// [`KeyringSecrets::retry_stale`]): the Secret Service's can, the macOS
/// keychain has no connection.
const PLATFORM_STORE_CAN_GO_STALE: bool = cfg!(all(
    unix,
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "android"
    ))
));

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
/// without restarting the app. On Linux a request that fails on a
/// connection opened by an earlier request is retried once on a new one:
/// the old connection's session belongs to the provider process that was
/// running then, so the first request after the provider restarted
/// (`KeePassXC` closed and opened again, say) would fail otherwise.
///
/// # Blocking
///
/// Every call blocks: the OS may ask the user to unlock the keyring or to
/// allow access, and the call waits for the answer. Call it from a thread
/// that may block (for example `tokio::task::spawn_blocking`), never from
/// the UI thread.
///
/// Calls for the same account run one at a time. A call waits up to two
/// seconds for an earlier one on its account (normally done in
/// milliseconds) and then fails, so retries don't pile up behind a prompt
/// nobody answers, and one account never has two prompts open. Calls for
/// different accounts don't wait for each other here; on Linux, though,
/// the Secret Service library handles one request at a time, so while it
/// waits for an unlock prompt every request waits.
pub struct KeyringSecrets {
    service: String,
    store_name: &'static str,
    connect: Box<Connect>,
    /// Whether a platform failure on a store opened by an earlier call is
    /// retried once on a newly opened store. Only for stores with a
    /// connection that can go stale: on macOS a platform failure may be
    /// the user's answer (a denied or cancelled access prompt), and
    /// retrying would ask again.
    retry_stale: bool,
    /// The open store. The lock is only held to read or replace it.
    store: Mutex<Option<Arc<CredentialStore>>>,
    /// The accounts with a call in progress.
    busy: Mutex<HashSet<String>>,
    /// Signalled whenever a call ends.
    idle: Condvar,
    /// How long a call waits for an earlier call on its account.
    busy_wait: Duration,
}

impl KeyringSecrets {
    /// Secrets in the OS credential store.
    pub fn new() -> Self {
        Self::with_connect(
            platform_store_name(),
            Box::new(platform_store),
            PLATFORM_STORE_CAN_GO_STALE,
        )
    }

    /// Secrets in `store`, for example `keyring_core::mock::Store` in tests
    /// or another keyring-compatible store.
    pub fn with_store(store: Arc<CredentialStore>) -> Self {
        Self::with_connect(
            "credential store",
            Box::new(move || Ok(Arc::clone(&store))),
            false,
        )
    }

    fn with_connect(store_name: &'static str, connect: Box<Connect>, retry_stale: bool) -> Self {
        Self {
            service: SERVICE.to_owned(),
            store_name,
            connect,
            retry_stale,
            store: Mutex::new(None),
            busy: Mutex::new(HashSet::new()),
            idle: Condvar::new(),
            busy_wait: BUSY_WAIT,
        }
    }

    /// The service name secrets are stored under ([`SERVICE`]).
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Runs `operation` on the entry for `account`, during the account's
    /// turn.
    fn with_entry<T>(
        &self,
        verb: Verb,
        account: &str,
        operation: impl Fn(&Entry) -> Result<T, KeyringError>,
    ) -> Result<T, SecretError> {
        if account.is_empty() {
            return Err(SecretError::new("the account (environment id) is empty"));
        }
        let _turn = self.claim(verb, account)?;
        let (store, opened_earlier) = self.open()?;
        let outcome = match self.attempt(&store, account, &operation) {
            Err(KeyringError::PlatformFailure(source)) if opened_earlier && self.retry_stale => {
                tracing::debug!(
                    store = self.store_name,
                    account,
                    operation = verb.as_str(),
                    error = %source,
                    "credential store request failed; retrying on a new connection"
                );
                let (store, _) = self.open()?;
                self.attempt(&store, account, &operation)
            }
            outcome => outcome,
        };
        outcome.map_err(|error| self.failed(verb, account, error))
    }

    /// One try on `store`. A platform failure drops the store, so the next
    /// request opens a new one.
    fn attempt<T>(
        &self,
        store: &Arc<CredentialStore>,
        account: &str,
        operation: &impl Fn(&Entry) -> Result<T, KeyringError>,
    ) -> Result<T, KeyringError> {
        let outcome = store
            .build(&self.service, account, None)
            .and_then(|entry| operation(&entry));
        if matches!(outcome, Err(KeyringError::PlatformFailure(_))) {
            // Possibly a dead D-Bus connection or a restarted daemon.
            self.forget(store);
        }
        outcome
    }

    /// The open store, and whether an earlier call opened it. It is opened
    /// without holding the lock: connecting can be slow, and calls for
    /// other accounts shouldn't wait for it.
    fn open(&self) -> Result<(Arc<CredentialStore>, bool), SecretError> {
        let cached = self.lock_store().clone();
        if let Some(store) = cached {
            return Ok((store, true));
        }
        let store = (self.connect)().map_err(|error| self.open_failed(&error))?;
        // Another call may have opened one meanwhile; keep the first.
        let store = Arc::clone(self.lock_store().get_or_insert(store));
        Ok((store, false))
    }

    /// Drops `store` unless another call has replaced it already.
    fn forget(&self, store: &Arc<CredentialStore>) {
        let mut slot = self.lock_store();
        if slot.as_ref().is_some_and(|open| Arc::ptr_eq(open, store)) {
            *slot = None;
        }
    }

    /// Waits for `account`'s turn, at most [`KeyringSecrets::busy_wait`].
    fn claim(&self, verb: Verb, account: &str) -> Result<Turn<'_>, SecretError> {
        let started = Instant::now();
        let mut busy = self.lock_busy();
        while busy.contains(account) {
            let left = self.busy_wait.saturating_sub(started.elapsed());
            if left.is_zero() {
                tracing::debug!(
                    store = self.store_name,
                    account,
                    operation = verb.as_str(),
                    "an earlier request for the account is still running"
                );
                return Err(SecretError::new(format!(
                    "cannot {} the password {} the {}: an earlier request for this \
                     environment is still waiting for an answer (is an unlock or access \
                     prompt open?)",
                    verb.as_str(),
                    verb.preposition(),
                    self.store_name
                )));
            }
            busy = self
                .idle
                .wait_timeout(busy, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        busy.insert(account.to_owned());
        Ok(Turn {
            secrets: self,
            account: account.to_owned(),
        })
    }

    fn lock_store(&self) -> MutexGuard<'_, Option<Arc<CredentialStore>>> {
        self.store.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_busy(&self) -> MutexGuard<'_, HashSet<String>> {
        self.busy.lock().unwrap_or_else(PoisonError::into_inner)
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

/// An account's turn in [`KeyringSecrets`]; it ends when dropped, also
/// when the operation panics.
struct Turn<'a> {
    secrets: &'a KeyringSecrets,
    account: String,
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.secrets.lock_busy().remove(&self.account);
        self.secrets.idle.notify_all();
    }
}

impl Default for KeyringSecrets {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for KeyringSecrets {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never wait for a lock here.
        let state = match self.store.try_lock() {
            Ok(slot) => open_state(slot.as_ref()),
            Err(TryLockError::Poisoned(poisoned)) => open_state(poisoned.into_inner().as_ref()),
            Err(TryLockError::WouldBlock) => "busy",
        };
        let in_progress = match self.busy.try_lock() {
            Ok(busy) => Some(busy.len()),
            Err(TryLockError::Poisoned(poisoned)) => Some(poisoned.into_inner().len()),
            Err(TryLockError::WouldBlock) => None,
        };
        let mut debug = f.debug_struct("KeyringSecrets");
        debug
            .field("service", &self.service)
            .field("store", &self.store_name)
            .field("state", &state);
        if let Some(count) = in_progress {
            debug.field("requests_in_progress", &count);
        }
        debug.finish_non_exhaustive()
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
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;

    use keyring_core::api::{CredentialApi, CredentialStoreApi};
    use keyring_core::{Credential, mock};

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

    /// Secrets in the mock `store`, opened through a connect function that
    /// counts how often it runs, with the given retry policy.
    fn counted(store: &Arc<mock::Store>, retry_stale: bool) -> (KeyringSecrets, Arc<AtomicUsize>) {
        let opened = Arc::new(AtomicUsize::new(0));
        let secrets = {
            let opened = Arc::clone(&opened);
            let store = store.clone();
            KeyringSecrets::with_connect(
                "Secret Service",
                Box::new(move || {
                    opened.fetch_add(1, Ordering::SeqCst);
                    let store: Arc<CredentialStore> = store.clone();
                    Ok(store)
                }),
                retry_stale,
            )
        };
        (secrets, opened)
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
                true,
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

    /// Without retries (the macOS keychain, where a platform failure may
    /// be a denied prompt), the failure is reported and the store is
    /// opened again by the next request.
    #[test]
    fn a_platform_failure_reopens_the_store() {
        let store = mock_store();
        let (secrets, attempts) = counted(&store, false);
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
    fn a_stale_connection_is_retried_on_a_new_one() {
        let store = mock_store();
        let (secrets, opened) = counted(&store, true);
        secrets.set("env", &secret("pw")).unwrap();
        assert_eq!(opened.load(Ordering::SeqCst), 1);

        // The provider restarted: the session of the open store is gone.
        // The request still succeeds, on a new connection.
        inject(
            &cred(&store, "env"),
            KeyringError::PlatformFailure(platform_error("No such session")),
        );
        assert_eq!(secrets.get("env").unwrap().unwrap().expose_secret(), "pw");
        assert_eq!(opened.load(Ordering::SeqCst), 2);

        // Writes and deletes too (they are idempotent, so retrying is safe).
        inject(
            &cred(&store, "env"),
            KeyringError::PlatformFailure(platform_error("No such session")),
        );
        secrets.set("env", &secret("new")).unwrap();
        inject(
            &cred(&store, "env"),
            KeyringError::PlatformFailure(platform_error("No such session")),
        );
        secrets.delete("env").unwrap();
        assert!(secrets.get("env").unwrap().is_none());
        assert_eq!(opened.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn a_failure_on_a_new_connection_is_not_retried() {
        let store = mock_store();
        let (secrets, opened) = counted(&store, true);
        cred(&store, "env").set_password("pw").unwrap();

        // Opened by this very request: retrying would only repeat it.
        inject(
            &cred(&store, "env"),
            KeyringError::PlatformFailure(platform_error("boom")),
        );
        let error = secrets.get("env").unwrap_err();
        assert!(error.message.contains("boom"), "{error}");
        assert_eq!(opened.load(Ordering::SeqCst), 1);

        // The next request connects again.
        assert_eq!(secrets.get("env").unwrap().unwrap().expose_secret(), "pw");
        assert_eq!(opened.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_reconnect_is_reported() {
        let store = mock_store();
        let unavailable = Arc::new(AtomicBool::new(false));
        let secrets = {
            let store = store.clone();
            let unavailable = Arc::clone(&unavailable);
            KeyringSecrets::with_connect(
                "Secret Service",
                Box::new(move || {
                    if unavailable.load(Ordering::SeqCst) {
                        return Err(KeyringError::PlatformFailure(platform_error(
                            "org.freedesktop.DBus.Error.ServiceUnknown",
                        )));
                    }
                    let store: Arc<CredentialStore> = store.clone();
                    Ok(store)
                }),
                true,
            )
        };
        cred(&store, "env").set_password("pw").unwrap();
        secrets.get("env").unwrap();

        // The provider went away for good: one retry, then its error.
        inject(
            &cred(&store, "env"),
            KeyringError::PlatformFailure(platform_error("No such session")),
        );
        unavailable.store(true, Ordering::SeqCst);
        let error = secrets.get("env").unwrap_err();
        assert!(
            error
                .message
                .starts_with("cannot open the Secret Service: "),
            "{error}"
        );
        assert!(error.message.contains("ServiceUnknown"), "{error}");

        // Back again: the next request connects.
        unavailable.store(false, Ordering::SeqCst);
        assert_eq!(secrets.get("env").unwrap().unwrap().expose_secret(), "pw");
    }

    #[test]
    fn denied_or_locked_is_not_retried() {
        let store = mock_store();
        let (secrets, opened) = counted(&store, true);
        secrets.set("env", &secret("pw")).unwrap();
        inject(
            &cred(&store, "env"),
            KeyringError::NoStorageAccess(platform_error("prompt dismissed")),
        );
        let error = secrets.get("env").unwrap_err();
        assert!(error.message.contains("prompt dismissed"), "{error}");
        assert_eq!(opened.load(Ordering::SeqCst), 1, "kept the store");
    }

    /// A gate requests wait at, like a prompt nobody has answered yet.
    #[derive(Default)]
    struct Prompt {
        answered: Mutex<bool>,
        answer: Condvar,
        /// Requests that reached the store and wait.
        waiting: AtomicUsize,
    }

    impl Prompt {
        fn wait(&self) {
            self.waiting.fetch_add(1, Ordering::SeqCst);
            let mut answered = self.answered.lock().unwrap();
            while !*answered {
                answered = self.answer.wait(answered).unwrap();
            }
            self.waiting.fetch_sub(1, Ordering::SeqCst);
        }

        fn answer(&self) {
            *self.answered.lock().unwrap() = true;
            self.answer.notify_all();
        }

        fn wait_for_requests(&self, count: usize) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while self.waiting.load(Ordering::SeqCst) < count {
                assert!(Instant::now() < deadline, "no request reached the store");
                thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// The mock store, except that every request for the account
    /// `"locked"` waits for [`Prompt::answer`] first.
    struct PromptingStore {
        mock: Arc<mock::Store>,
        prompt: Arc<Prompt>,
    }

    struct PromptingCred {
        inner: Entry,
        prompt: Arc<Prompt>,
    }

    impl CredentialStoreApi for PromptingStore {
        fn vendor(&self) -> String {
            "prompting test store".to_owned()
        }

        fn id(&self) -> String {
            "prompting".to_owned()
        }

        fn build(
            &self,
            service: &str,
            user: &str,
            modifiers: Option<&HashMap<&str, &str>>,
        ) -> keyring_core::Result<Entry> {
            let inner = self.mock.build(service, user, modifiers)?;
            if user != "locked" {
                return Ok(inner);
            }
            let cred: Arc<Credential> = Arc::new(PromptingCred {
                inner,
                prompt: Arc::clone(&self.prompt),
            });
            Ok(Entry::new_with_credential(cred))
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl CredentialApi for PromptingCred {
        fn set_secret(&self, secret: &[u8]) -> keyring_core::Result<()> {
            self.prompt.wait();
            self.inner.set_secret(secret)
        }

        fn get_secret(&self) -> keyring_core::Result<Vec<u8>> {
            self.prompt.wait();
            self.inner.get_secret()
        }

        fn delete_credential(&self) -> keyring_core::Result<()> {
            self.prompt.wait();
            self.inner.delete_credential()
        }

        fn get_credential(&self) -> keyring_core::Result<Option<Arc<Credential>>> {
            Ok(None)
        }

        fn get_specifiers(&self) -> Option<(String, String)> {
            None
        }

        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
    }

    impl fmt::Debug for PromptingCred {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("PromptingCred")
        }
    }

    fn prompting(busy_wait: Duration) -> (Arc<KeyringSecrets>, Arc<Prompt>, Arc<mock::Store>) {
        let mock = mock_store();
        let prompt = Arc::new(Prompt::default());
        let store: Arc<CredentialStore> = Arc::new(PromptingStore {
            mock: mock.clone(),
            prompt: Arc::clone(&prompt),
        });
        let mut secrets = KeyringSecrets::with_store(store);
        secrets.busy_wait = busy_wait;
        (Arc::new(secrets), prompt, mock)
    }

    #[test]
    fn a_pending_prompt_does_not_hold_up_other_accounts() {
        let (secrets, prompt, mock) = prompting(Duration::from_millis(200));
        cred(&mock, "locked").set_password("pw").unwrap();
        secrets.set("other", &secret("other-pw")).unwrap();

        let pending = {
            let secrets = Arc::clone(&secrets);
            thread::spawn(move || secrets.get("locked"))
        };
        prompt.wait_for_requests(1);

        let started = Instant::now();
        assert_eq!(
            secrets.get("other").unwrap().unwrap().expose_secret(),
            "other-pw"
        );
        secrets.set("third", &secret("3")).unwrap();
        secrets.delete("third").unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "other accounts waited"
        );
        assert!(format!("{secrets:?}").contains("requests_in_progress: 1"));

        prompt.answer();
        let answered = pending.join().unwrap().unwrap().unwrap();
        assert_eq!(answered.expose_secret(), "pw");
        assert!(format!("{secrets:?}").contains("requests_in_progress: 0"));
    }

    #[test]
    fn requests_for_an_account_with_a_pending_prompt_fail_fast() {
        let (secrets, prompt, mock) = prompting(Duration::from_millis(100));
        cred(&mock, "locked").set_password("pw").unwrap();
        let pending = {
            let secrets = Arc::clone(&secrets);
            thread::spawn(move || secrets.get("locked"))
        };
        prompt.wait_for_requests(1);

        // A retry doesn't queue up (no second prompt, no parked thread):
        // it fails once the short wait is over.
        let started = Instant::now();
        let error = secrets.set("locked", &secret("new")).unwrap_err();
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(100) && waited < Duration::from_secs(5),
            "{waited:?}"
        );
        assert!(
            error
                .message
                .starts_with("cannot save the password in the credential store: "),
            "{error}"
        );
        assert!(error.message.contains("still waiting"), "{error}");
        assert_eq!(
            prompt.waiting.load(Ordering::SeqCst),
            1,
            "never reached the store"
        );

        prompt.answer();
        assert_eq!(
            pending.join().unwrap().unwrap().unwrap().expose_secret(),
            "pw"
        );
        // Answered: the account is free again.
        secrets.set("locked", &secret("new")).unwrap();
        assert_eq!(
            secrets.get("locked").unwrap().unwrap().expose_secret(),
            "new"
        );
    }

    #[test]
    fn a_short_overlap_on_one_account_waits_its_turn() {
        let (secrets, prompt, mock) = prompting(Duration::from_secs(10));
        cred(&mock, "locked").set_password("pw").unwrap();
        let first = {
            let secrets = Arc::clone(&secrets);
            thread::spawn(move || secrets.get("locked"))
        };
        prompt.wait_for_requests(1);
        let second = {
            let secrets = Arc::clone(&secrets);
            thread::spawn(move || secrets.set("locked", &secret("new")))
        };
        thread::sleep(Duration::from_millis(50));
        assert_eq!(prompt.waiting.load(Ordering::SeqCst), 1, "one at a time");
        prompt.answer();
        assert_eq!(
            first.join().unwrap().unwrap().unwrap().expose_secret(),
            "pw"
        );
        second.join().unwrap().unwrap();
        assert_eq!(
            secrets.get("locked").unwrap().unwrap().expose_secret(),
            "new"
        );
    }

    #[test]
    fn concurrent_requests_stay_consistent() {
        let store = mock_store();
        let secrets = Arc::new(secrets(&store));
        let workers: Vec<_> = (0..8)
            .map(|worker| {
                let secrets = Arc::clone(&secrets);
                thread::spawn(move || {
                    for round in 0..50 {
                        let account = format!("env-{}", (worker + round) % 4);
                        secrets.set(&account, &secret(&account)).unwrap();
                        let read = secrets.get(&account).unwrap();
                        // Others may have deleted it meanwhile, never
                        // replaced it with something else.
                        if let Some(read) = read {
                            assert_eq!(read.expose_secret(), account);
                        }
                        if round % 7 == 0 {
                            secrets.delete(&account).unwrap();
                        }
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        assert!(format!("{secrets:?}").contains("requests_in_progress: 0"));
    }

    #[test]
    fn a_panicking_request_frees_its_account() {
        let store = mock_store();
        let secrets = secrets(&store);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            secrets.with_entry(Verb::Read, "env", |_| -> Result<(), KeyringError> {
                panic!("store bug")
            })
        }));
        assert!(outcome.is_err());
        secrets.set("env", &secret("pw")).unwrap();
        assert_eq!(secrets.get("env").unwrap().unwrap().expose_secret(), "pw");
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
        let handle = thread::spawn(move || port.get("env").unwrap().is_some());
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
