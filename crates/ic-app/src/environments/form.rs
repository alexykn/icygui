//! The environment editor's form (ENV-02) as plain data: the fields as
//! typed, the environment they make, and the problems to show next to
//! each field. Pure, so it's tested without a window.

use std::collections::BTreeMap;
use std::path::PathBuf;

use ic_config::{ApiUrl, AuthConfig, Environment, MAX_API_URLS, TlsConfig};

/// A field of the form, for placing its problem.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum FormField {
    /// The display name.
    Name,
    /// The URL list as a whole (at least one URL, at most
    /// [`MAX_API_URLS`]).
    Urls,
    /// The API URL at this position of the list (ENV-12).
    Url(usize),
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
    /// The pinned SHA-256 fingerprint of the URL at this position.
    Pin(usize),
    /// The TLS server-name override of the URL at this position.
    ServerName(usize),
}

impl FormField {
    /// A TLS field (shown in the folded TLS settings).
    pub(crate) fn is_tls(self) -> bool {
        matches!(self, Self::CaFile | Self::Pin(_) | Self::ServerName(_))
    }
}

/// One API URL as typed, with its pin and server name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct UrlForm {
    /// The URL.
    pub(crate) url: String,
    /// Pinned SHA-256 fingerprint.
    pub(crate) pinned: String,
    /// TLS server-name override.
    pub(crate) server_name: String,
}

impl UrlForm {
    /// A row for `url` without pin or server name.
    pub(crate) fn new(url: &str) -> Self {
        Self {
            url: url.to_owned(),
            ..Self::default()
        }
    }

    fn of(url: &ApiUrl) -> Self {
        Self {
            url: url.url.clone(),
            pinned: url.pinned_sha256.clone().unwrap_or_default(),
            server_name: url.server_name.clone().unwrap_or_default(),
        }
    }

    /// The URL it describes: trimmed, blank optional fields unset, a valid
    /// fingerprint in its canonical form.
    fn to_url(&self) -> ApiUrl {
        let pinned = optional(&self.pinned).map(|pin| {
            ic_config::parse_fingerprint(&pin)
                .map_or(pin, |bytes| ic_config::format_fingerprint(&bytes))
        });
        ApiUrl {
            url: self.url.trim().to_owned(),
            pinned_sha256: pinned,
            server_name: optional(&self.server_name),
        }
    }
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
    /// The API URLs in order of preference, each with its pin and server
    /// name (ENV-12).
    pub(crate) urls: Vec<UrlForm>,
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
            urls: vec![UrlForm::new("https://")],
            auth: AuthKind::Password,
            username: String::new(),
            password_typed: false,
            cert_path: String::new(),
            key_path: String::new(),
            author: String::new(),
            ca_file: String::new(),
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
            urls: environment.urls.iter().map(UrlForm::of).collect(),
            auth,
            username,
            password_typed: false,
            cert_path,
            key_path,
            author: environment.author.clone().unwrap_or_default(),
            ca_file: tls.ca_file.as_ref().map(path).unwrap_or_default(),
            use_system_roots: tls.use_system_roots,
        }
    }

    /// Whether this edits an existing environment.
    pub(crate) fn is_new(&self) -> bool {
        self.base.is_none()
    }

    /// Whether another URL may be added ([`MAX_API_URLS`]).
    pub(crate) fn can_add_url(&self) -> bool {
        self.urls.len() < MAX_API_URLS
    }

    /// Adds an empty URL row at the end; returns its position.
    pub(crate) fn add_url(&mut self) -> Option<usize> {
        if !self.can_add_url() {
            return None;
        }
        self.urls.push(UrlForm::new("https://"));
        Some(self.urls.len() - 1)
    }

    /// Removes the URL at `index` (the last one stays: an environment has
    /// at least one).
    pub(crate) fn remove_url(&mut self, index: usize) -> bool {
        if self.urls.len() <= 1 || index >= self.urls.len() {
            return false;
        }
        self.urls.remove(index);
        true
    }

    /// Moves the URL at `index` one place up (earlier: preferred) or down.
    pub(crate) fn move_url(&mut self, index: usize, up: bool) -> bool {
        let Some(other) = (if up {
            index.checked_sub(1)
        } else {
            Some(index + 1)
        }) else {
            return false;
        };
        if index >= self.urls.len() || other >= self.urls.len() {
            return false;
        }
        self.urls.swap(index, other);
        true
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
            let mut fresh = Environment::new(&self.name, "", auth.clone());
            fresh.id.clone_from(&self.id);
            fresh
        };
        self.name.trim().clone_into(&mut environment.name);
        environment.urls = self.urls.iter().map(UrlForm::to_url).collect();
        environment.auth = auth;
        environment.author = optional(&self.author);
        environment.tls = TlsConfig {
            ca_file: optional(&self.ca_file).map(|path| expand_home(&path)),
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
    if let Some(rest) = path.strip_prefix("urls[") {
        let (index, field) = rest.split_once("].")?;
        let index: usize = index.parse().ok()?;
        return Some(match field {
            "url" => FormField::Url(index),
            "pinned_sha256" => FormField::Pin(index),
            "server_name" => FormField::ServerName(index),
            _ => return None,
        });
    }
    Some(match path {
        "name" => FormField::Name,
        "urls" => FormField::Urls,
        "auth.username" => FormField::Username,
        "auth.cert_path" => FormField::CertPath,
        "auth.key_path" => FormField::KeyPath,
        "author" => FormField::Author,
        "tls.ca_file" => FormField::CaFile,
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
            urls: vec![UrlForm::new(" https://master-01:5665 ")],
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
        assert_eq!(environment.urls, [ApiUrl::new("https://master-01:5665")]);
        assert_eq!(environment.groups.len(), 1, "the default dashboards");
        assert_eq!(environment.groups[0].dashboards.len(), 3);
        assert!(environment.tls.use_system_roots);
        assert_eq!(environment.author, None);
        assert!(form.issues().is_empty(), "{:?}", form.issues());
    }

    #[test]
    fn missing_and_invalid_fields_are_named() {
        let form = EnvironmentForm {
            urls: vec![UrlForm {
                url: "http://master-01:5665".to_owned(),
                pinned: "not a fingerprint".to_owned(),
                server_name: "https://x".to_owned(),
            }],
            ..EnvironmentForm::new_environment()
        };
        let issues = form.issues();
        assert!(issues.contains_key(&FormField::Name));
        assert!(issues[&FormField::Url(0)].contains("https"), "{issues:?}");
        assert!(issues.contains_key(&FormField::Username));
        assert!(issues.contains_key(&FormField::Password));
        assert!(issues.contains_key(&FormField::Pin(0)));
        assert!(issues.contains_key(&FormField::ServerName(0)));
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
        form.urls[0].pinned = "ab".repeat(32);
        let edited = form.to_environment();
        assert_eq!(edited.id, original.id);
        assert_eq!(edited.groups, original.groups);
        assert_eq!(edited.name, "production");
        assert_eq!(
            edited.urls[0].pinned_sha256.as_deref(),
            Some(ic_config::format_fingerprint(&[0xab; 32]).as_str()),
            "fingerprints are written canonically"
        );
        assert_eq!(EnvironmentForm::edit(&edited).urls[0].pinned.len(), 95);
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
        assert_eq!(field_of("urls[1].pinned_sha256"), Some(FormField::Pin(1)));
        assert_eq!(field_of("urls[0].url"), Some(FormField::Url(0)));
        assert_eq!(
            field_of("urls[12].server_name"),
            Some(FormField::ServerName(12))
        );
        assert_eq!(field_of("urls"), Some(FormField::Urls));
        assert_eq!(field_of("urls[x].url"), None);
        assert_eq!(field_of("groups[0].name"), None);
    }

    #[test]
    fn urls_are_added_moved_and_removed_in_order_of_preference() {
        let mut form = filled();
        assert_eq!(form.add_url(), Some(1));
        form.urls[1].url = "https://master-02:5665".to_owned();
        form.urls[1].pinned = "ab".repeat(32);
        assert!(form.issues().is_empty(), "{:?}", form.issues());
        let environment = form.to_environment();
        assert_eq!(environment.urls.len(), 2);
        assert_eq!(environment.urls[1].url, "https://master-02:5665");
        assert!(environment.urls[1].pinned_sha256.is_some());
        assert_eq!(environment.urls[0].pinned_sha256, None, "pins are per URL");

        assert!(form.move_url(1, true));
        assert_eq!(form.urls[0].url, "https://master-02:5665");
        assert!(!form.move_url(0, true), "already first");
        assert!(!form.move_url(1, false), "already last");
        assert!(form.move_url(0, false));
        assert_eq!(form.urls[1].url, "https://master-02:5665");

        // The same URL twice is named where it repeats.
        form.urls[1].url = "https://master-01:5665".to_owned();
        assert!(form.issues()[&FormField::Url(1)].contains("already listed"));

        assert!(form.remove_url(1));
        assert!(!form.remove_url(0), "the last URL stays");
        assert_eq!(form.urls.len(), 1);
        for _ in 1..MAX_API_URLS {
            form.add_url();
        }
        assert!(!form.can_add_url());
        assert_eq!(form.add_url(), None);
    }

    #[test]
    fn editing_reads_every_url_with_its_pin() {
        let mut environment = filled().to_environment();
        environment.urls.push(ApiUrl {
            pinned_sha256: Some("AB".to_owned()),
            server_name: Some("master-02".to_owned()),
            ..ApiUrl::new("https://10.0.0.2:5665")
        });
        let form = EnvironmentForm::edit(&environment);
        assert_eq!(form.urls.len(), 2);
        assert_eq!(form.urls[1].pinned, "AB");
        assert_eq!(form.urls[1].server_name, "master-02");
        assert_eq!(form.to_environment().urls, environment.urls);
    }
}
