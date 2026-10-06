//! The environment editor's form (ENV-02) as plain data: the fields as
//! typed, the environment they make, and the problems to show next to
//! each field. Pure, so it's tested without a window.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ic_config::{AuthConfig, Environment, TlsConfig};

/// A field of the form, for placing its problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum FormField {
    /// The display name.
    Name,
    /// The API URL.
    Url,
    /// The basic-auth user name.
    Username,
    /// The basic-auth password.
    Password,
    /// The client certificate file.
    CertPath,
    /// The client key file.
    KeyPath,
    /// The author name for actions.
    Author,
    /// The CA file.
    CaFile,
    /// The pinned SHA-256 fingerprint.
    Pin,
    /// The TLS server-name override.
    ServerName,
}

/// How the client logs in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AuthKind {
    /// API user and password (the password in the keychain).
    #[default]
    Password,
    /// A TLS client certificate and key.
    Certificate,
}

/// The editor's fields as typed. The password itself never lives here: it
/// stays in its (masked) text field until it goes to the keychain; the
/// form only knows whether one was typed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EnvironmentForm {
    /// The environment being edited (`None`: a new one).
    pub(crate) base: Option<Environment>,
    /// Its id: the edited one's, or a fresh one (fixed for the dialog's
    /// life, so a tested password and the saved one agree).
    pub(crate) id: String,
    /// Display name.
    pub(crate) name: String,
    /// API URL.
    pub(crate) url: String,
    /// How it logs in.
    pub(crate) auth: AuthKind,
    /// API user name.
    pub(crate) username: String,
    /// Whether a password was typed.
    pub(crate) password_typed: bool,
    /// Client certificate file.
    pub(crate) cert_path: String,
    /// Client key file.
    pub(crate) key_path: String,
    /// Author name (blank: the API user).
    pub(crate) author: String,
    /// CA file.
    pub(crate) ca_file: String,
    /// Pinned SHA-256 fingerprint.
    pub(crate) pinned: String,
    /// TLS server-name override.
    pub(crate) server_name: String,
    /// Trust the system's root certificates.
    pub(crate) use_system_roots: bool,
}

impl EnvironmentForm {
    /// The form for a new environment: no name or URL yet, password login,
    /// the system's root certificates trusted.
    pub(crate) fn new_environment() -> Self {
        Self {
            base: None,
            id: ic_config::new_id(),
            name: String::new(),
            url: "https://".to_owned(),
            auth: AuthKind::Password,
            username: String::new(),
            password_typed: false,
            cert_path: String::new(),
            key_path: String::new(),
            author: String::new(),
            ca_file: String::new(),
            pinned: String::new(),
            server_name: String::new(),
            use_system_roots: true,
        }
    }

    /// The form for editing `environment`.
    pub(crate) fn edit(environment: &Environment) -> Self {
        let path = |path: &PathBuf| path.display().to_string();
        let (auth, username, cert_path, key_path) = match &environment.auth {
            AuthConfig::Basic { username } => (
                AuthKind::Password,
                username.clone(),
                String::new(),
                String::new(),
            ),
            AuthConfig::ClientCertificate {
                cert_path,
                key_path,
            } => (
                AuthKind::Certificate,
                String::new(),
                path(cert_path),
                path(key_path),
            ),
        };
        let tls = &environment.tls;
        Self {
            base: Some(environment.clone()),
            id: environment.id.clone(),
            name: environment.name.clone(),
            url: environment.url.clone(),
            auth,
            username,
            password_typed: false,
            cert_path,
            key_path,
            author: environment.author.clone().unwrap_or_default(),
            ca_file: tls.ca_file.as_ref().map(path).unwrap_or_default(),
            pinned: tls.pinned_sha256.clone().unwrap_or_default(),
            server_name: tls.server_name.clone().unwrap_or_default(),
            use_system_roots: tls.use_system_roots,
        }
    }

    /// Whether this edits an existing environment.
    pub(crate) fn is_new(&self) -> bool {
        self.base.is_none()
    }

    /// The environment the form describes: the edited one with the form's
    /// connection settings (its dashboards and rules stay), or a new one
    /// with the default dashboards (DASH-05). Text is trimmed, blank
    /// optional fields are unset, `~/` in paths is the home directory, and
    /// a valid fingerprint is written in its canonical form.
    pub(crate) fn to_environment(&self) -> Environment {
        let auth = match self.auth {
            AuthKind::Password => AuthConfig::Basic {
                username: self.username.trim().to_owned(),
            },
            AuthKind::Certificate => AuthConfig::ClientCertificate {
                cert_path: expand_home(&self.cert_path),
                key_path: expand_home(&self.key_path),
            },
        };
        let mut environment = if let Some(base) = &self.base {
            base.clone()
        } else {
            let mut fresh = Environment::new(&self.name, &self.url, auth.clone());
            fresh.id.clone_from(&self.id);
            fresh
        };
        self.name.trim().clone_into(&mut environment.name);
        self.url.trim().clone_into(&mut environment.url);
        environment.auth = auth;
        environment.author = optional(&self.author);
        let pinned = optional(&self.pinned).map(|pin| {
            ic_config::parse_fingerprint(&pin)
                .map_or(pin, |bytes| ic_config::format_fingerprint(&bytes))
        });
        environment.tls = TlsConfig {
            ca_file: optional(&self.ca_file).map(|path| expand_home(&path)),
            pinned_sha256: pinned,
            server_name: optional(&self.server_name),
            use_system_roots: self.use_system_roots,
        };
        environment
    }

    /// The problems to show, at most one per field (the first found):
    /// everything `Environment::validate` reports for the connection
    /// settings, and a missing password for a new environment that logs in
    /// with one.
    pub(crate) fn issues(&self) -> BTreeMap<FormField, String> {
        let mut issues = BTreeMap::new();
        for issue in self.to_environment().validate() {
            let Some(field) = field_of(&issue.path) else {
                continue;
            };
            issues.entry(field).or_insert(issue.message);
        }
        if self.is_new() && self.auth == AuthKind::Password && !self.password_typed {
            issues.insert(
                FormField::Password,
                "must not be empty; icygui keeps it in the system keychain".to_owned(),
            );
        }
        issues
    }
}

/// The form field a validation path (relative to the environment) is
/// about.
pub(crate) fn field_of(path: &str) -> Option<FormField> {
    Some(match path {
        "name" => FormField::Name,
        "url" => FormField::Url,
        "auth.username" => FormField::Username,
        "auth.cert_path" => FormField::CertPath,
        "auth.key_path" => FormField::KeyPath,
        "author" => FormField::Author,
        "tls.ca_file" => FormField::CaFile,
        "tls.pinned_sha256" => FormField::Pin,
        "tls.server_name" => FormField::ServerName,
        _ => return None,
    })
}

/// `text` trimmed, unless blank.
fn optional(text: &str) -> Option<String> {
    Some(text.trim().to_owned()).filter(|text| !text.is_empty())
}

/// A path as typed, with a leading `~/` (or a lone `~`) meaning the home
/// directory.
fn expand_home(text: &str) -> PathBuf {
    let text = text.trim();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (text.strip_prefix("~/"), text == "~", home) {
        (Some(rest), _, Some(home)) => home.join(rest),
        (None, true, Some(home)) => home,
        _ => PathBuf::from(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filled() -> EnvironmentForm {
        EnvironmentForm {
            name: " prod ".to_owned(),
            url: " https://master-01:5665 ".to_owned(),
            username: "icygui".to_owned(),
            password_typed: true,
            ..EnvironmentForm::new_environment()
        }
    }

    #[test]
    fn a_new_environment_gets_the_default_dashboards_and_the_forms_id() {
        let form = filled();
        let environment = form.to_environment();
        assert_eq!(environment.id, form.id);
        assert_eq!(environment.name, "prod");
        assert_eq!(environment.url, "https://master-01:5665");
        assert_eq!(environment.groups.len(), 1, "the default dashboards");
        assert_eq!(environment.groups[0].dashboards.len(), 3);
        assert!(environment.tls.use_system_roots);
        assert_eq!(environment.author, None);
        assert!(form.issues().is_empty(), "{:?}", form.issues());
    }

    #[test]
    fn missing_and_invalid_fields_are_named() {
        let form = EnvironmentForm {
            url: "http://master-01:5665".to_owned(),
            pinned: "not a fingerprint".to_owned(),
            server_name: "https://x".to_owned(),
            ..EnvironmentForm::new_environment()
        };
        let issues = form.issues();
        assert!(issues.contains_key(&FormField::Name));
        assert!(issues[&FormField::Url].contains("https"), "{issues:?}");
        assert!(issues.contains_key(&FormField::Username));
        assert!(issues.contains_key(&FormField::Password));
        assert!(issues.contains_key(&FormField::Pin));
        assert!(issues.contains_key(&FormField::ServerName));
    }

    #[test]
    fn client_certificates_need_files_and_an_author() {
        let form = EnvironmentForm {
            auth: AuthKind::Certificate,
            cert_path: "relative/cert.pem".to_owned(),
            ..filled()
        };
        let issues = form.issues();
        assert!(
            issues[&FormField::CertPath].contains("absolute"),
            "{issues:?}"
        );
        assert!(issues.contains_key(&FormField::KeyPath));
        assert!(issues.contains_key(&FormField::Author));
        assert!(
            !issues.contains_key(&FormField::Password),
            "no password needed"
        );
        let complete = EnvironmentForm {
            cert_path: "/etc/icygui/cert.pem".to_owned(),
            key_path: "/etc/icygui/key.pem".to_owned(),
            author: "ops".to_owned(),
            ..form
        };
        assert!(complete.issues().is_empty(), "{:?}", complete.issues());
    }

    #[test]
    fn editing_keeps_dashboards_and_needs_no_new_password() {
        let mut original = filled().to_environment();
        original.groups.clear();
        original.tls.ca_file = Some(PathBuf::from("/etc/icinga2/ca.crt"));
        let mut form = EnvironmentForm::edit(&original);
        assert!(!form.is_new());
        assert_eq!(form.ca_file, "/etc/icinga2/ca.crt");
        assert!(form.issues().is_empty(), "the stored password stays");
        form.name = "production".to_owned();
        form.pinned = "ab".repeat(32);
        let edited = form.to_environment();
        assert_eq!(edited.id, original.id);
        assert_eq!(edited.groups, original.groups);
        assert_eq!(edited.name, "production");
        assert_eq!(
            edited.tls.pinned_sha256.as_deref(),
            Some(ic_config::format_fingerprint(&[0xab; 32]).as_str()),
            "fingerprints are written canonically"
        );
        assert_eq!(EnvironmentForm::edit(&edited).pinned.len(), 95);
    }

    #[test]
    fn home_directories_are_expanded() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
            return;
        };
        assert_eq!(expand_home("~/certs/ca.pem"), home.join("certs/ca.pem"));
        assert_eq!(expand_home(" ~ "), home);
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn validation_paths_map_to_fields() {
        assert_eq!(field_of("tls.pinned_sha256"), Some(FormField::Pin));
        assert_eq!(field_of("groups[0].name"), None);
    }
}
