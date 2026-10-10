//! Stores a password in the OS secret store exactly as icygui does
//! ([`ic_platform::KeyringSecrets`]: the service [`ic_platform::SERVICE`],
//! the environment id as the account), then reads it back. For headless
//! runs that start a Secret Service of their own, such as the demo
//! harness (`demo/screenshot.sh`: a D-Bus session with a throwaway GNOME
//! Keyring); an example, so it is never part of a release.
//!
//! ```text
//! printf %s "$PASSWORD" | cargo run -p ic-platform --example store_secret -- <environment id>
//! ```
//!
//! The password comes on stdin (one trailing line break is ignored), never
//! on the command line, where other users' `ps` could read it.

use std::io::Read as _;

use ic_core::ports::SecretStore as _;
use secrecy::{ExposeSecret as _, SecretString};

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (Some(account), None) = (args.next(), args.next()) else {
        return Err("usage: store_secret <environment id>, the password on stdin".to_owned());
    };
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|error| format!("cannot read the password from stdin: {error}"))?;
    let trimmed = text.strip_suffix('\n').unwrap_or(&text);
    let password = SecretString::from(trimmed.strip_suffix('\r').unwrap_or(trimmed).to_owned());
    if password.expose_secret().is_empty() {
        return Err("no password on stdin".to_owned());
    }
    let store = ic_platform::KeyringSecrets::new();
    store
        .set(&account, &password)
        .map_err(|error| error.to_string())?;
    match store.get(&account).map_err(|error| error.to_string())? {
        Some(read) if read.expose_secret() == password.expose_secret() => Ok(()),
        _ => Err("the secret store didn't give the password back".to_owned()),
    }
}
