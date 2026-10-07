//! Checks for settings that load fine but can't work: missing names, bad
//! URLs, duplicate ids, unparsable fingerprints and the like.
//!
//! Validation is advisory and pure (no file system access): the settings UI
//! shows the issues next to the fields named by their paths. Loading never
//! fails because of them. Saving refuses only the issues that would put a
//! secret into the file ([`secret_issues`]).

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::path::Path;

use ic_model::ObjectKey;
use ic_rules::{NotificationSettings, QuietHours};

use crate::environment::{CREDENTIALS_REASON, has_credentials, parse_api_url};
use crate::error::{ConfigError, MAX_VALUE_CHARS, excerpt};
use crate::fingerprint::parse_fingerprint;
use crate::model::{
    ApiUrl, AuthConfig, Config, Dashboard, DashboardGroup, Environment, MAX_API_URLS, TlsConfig,
};
use crate::view::{GroupBy, GroupSource, MAX_VIEWS, ObjectKind, STREAM_LINES, View, ViewDisplay};

/// The shortest allowed event log retention, in hours.
pub const MIN_EVENT_LOG_RETENTION_HOURS: u32 = 1;

/// The shortest allowed reconcile interval, in seconds, when one is set
/// (`0` means adaptive). Each reconcile is a lean reload of every object,
/// about 28 MB from a master with 30 000 services, so the engine never
/// reconciles more often than this even if the file says otherwise.
pub const MIN_RECONCILE_INTERVAL_SECS: u32 = 60;

const MINUTES_PER_DAY: u16 = 24 * 60;

/// Why a basic-auth username with a `:` is rejected. Icinga splits Basic
/// credentials at the first `:` (`ApiUser::GetByAuthHeader`), so such a
/// name never matches an API user; it is usually curl's `user:password`.
const USERNAME_COLON_REASON: &str =
    "must not contain `:`; enter the password separately, icygui keeps it in the system keychain";

/// One problem found by [`Config::validate`].
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ValidationIssue {
    /// Where the problem is, as a path of keys and indices into the
    /// settings: `environments[0].tls.pinned_sha256`. Paths from
    /// [`Environment::validate`] are relative to the environment
    /// (`tls.pinned_sha256`).
    pub path: String,
    /// What is wrong, phrased to follow the field's name
    /// (`must not be empty`).
    pub message: String,
}

impl fmt::Display for ValidationIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

impl Config {
    /// Checks the settings and returns every problem found; empty when the
    /// settings are usable.
    ///
    /// Checks app-wide preferences, that `active_environment` names an
    /// environment, that environment ids are unique, and everything
    /// [`Environment::validate`] checks for each environment.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Issues::default();
        if self.general.event_log_retention_hours < MIN_EVENT_LOG_RETENTION_HOURS {
            issues.push(
                "general.event_log_retention_hours",
                format!("must be at least {MIN_EVENT_LOG_RETENTION_HOURS} hour"),
            );
        }
        let reconcile = self.general.reconcile_interval_secs;
        if reconcile != 0 && reconcile < MIN_RECONCILE_INTERVAL_SECS {
            issues.push(
                "general.reconcile_interval_secs",
                format!("must be 0 (adaptive) or at least {MIN_RECONCILE_INTERVAL_SECS} seconds"),
            );
        }
        if let Some(active) = &self.active_environment
            && self.environment(active).is_none()
        {
            issues.push(
                "active_environment",
                format!(
                    "no environment has the id `{}`",
                    excerpt(active, MAX_VALUE_CHARS)
                ),
            );
        }
        let mut ids = UniqueIds::default();
        for (index, environment) in self.environments.iter().enumerate() {
            let path = format!("environments[{index}]");
            ids.check(&mut issues, &path, &environment.id);
            check_environment(environment, &path, &mut issues);
        }
        issues.0
    }
}

impl Environment {
    /// Checks one environment, as the environment editor needs before
    /// saving it. Paths are relative to the environment (`urls[0].url`,
    /// `auth.username`, `groups[0].name`).
    ///
    /// Checks that the id and name are set; that there are one to
    /// [`MAX_API_URLS`] URLs, each valid (see [`ApiUrl::api_url`]) and
    /// listed once, with a valid pinned fingerprint and server name if
    /// set; the username or certificate paths; that client-certificate
    /// authentication has an author; the CA file; group and dashboard ids
    /// (unique within the environment) and names; views; and the
    /// notification settings.
    pub fn validate(&self) -> Vec<ValidationIssue> {
        let mut issues = Issues::default();
        UniqueIds::default().check(&mut issues, "", &self.id);
        check_environment(self, "", &mut issues);
        issues.0
    }
}

/// Checks imported groups: ids, names and views. Paths are `groups[i]…`.
pub(crate) fn validate_groups(groups: &[DashboardGroup]) -> Vec<ValidationIssue> {
    let mut issues = Issues::default();
    check_groups(groups, "", &mut issues);
    issues.0
}

/// The issues that would put a secret into the settings file: a user name
/// or password in an environment's URL, or a basic-auth username with a
/// `:` (curl's `user:password`). [`ConfigStore::save`] refuses settings
/// with any of them; [`Config::validate`] reports them with the same paths
/// and messages.
///
/// [`ConfigStore::save`]: crate::ConfigStore::save
pub(crate) fn secret_issues(config: &Config) -> Vec<ValidationIssue> {
    let mut issues = Issues::default();
    for (index, environment) in config.environments.iter().enumerate() {
        let path = format!("environments[{index}]");
        for (url_index, url) in environment.urls.iter().enumerate() {
            if has_credentials(&url.url) {
                issues.push(
                    join(&path, &format!("urls[{url_index}].url")),
                    CREDENTIALS_REASON,
                );
            }
        }
        if let AuthConfig::Basic { username } = &environment.auth
            && username.contains(':')
        {
            issues.push(join(&path, "auth.username"), USERNAME_COLON_REASON);
        }
    }
    issues.0
}

#[derive(Default)]
struct Issues(Vec<ValidationIssue>);

impl Issues {
    fn push(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.0.push(ValidationIssue {
            path: path.into(),
            message: message.into(),
        });
    }
}

/// Tracks ids within one scope and reports blank and repeated ones at
/// `<path>.id`.
#[derive(Default)]
struct UniqueIds<'a> {
    seen: HashMap<&'a str, String>,
}

impl<'a> UniqueIds<'a> {
    fn check(&mut self, issues: &mut Issues, path: &str, id: &'a str) {
        let field = join(path, "id");
        if id.trim().is_empty() {
            issues.push(field, "must not be empty");
        } else if let Some(first) = self.seen.get(id) {
            issues.push(
                field,
                format!(
                    "`{}` is already used by {first}",
                    excerpt(id, MAX_VALUE_CHARS)
                ),
            );
        } else {
            self.seen.insert(id, path.to_owned());
        }
    }
}

/// `prefix.field`, or just `field` at the top.
fn join(prefix: &str, field: &str) -> String {
    if prefix.is_empty() {
        field.to_owned()
    } else {
        format!("{prefix}.{field}")
    }
}

fn check_environment(environment: &Environment, path: &str, issues: &mut Issues) {
    check_name(&environment.name, &join(path, "name"), issues);
    check_urls(&environment.urls, path, issues);
    check_auth(environment, path, issues);
    check_tls(&environment.tls, &join(path, "tls"), issues);
    check_groups(&environment.groups, path, issues);
    check_notifications(
        &environment.notifications,
        &join(path, "notifications"),
        issues,
    );
}

fn check_name(name: &str, path: &str, issues: &mut Issues) {
    if name.trim().is_empty() {
        issues.push(path, "must not be empty");
    }
}

fn check_auth(environment: &Environment, path: &str, issues: &mut Issues) {
    match &environment.auth {
        AuthConfig::Basic { username } => {
            if let Some(reason) = username_problem(username) {
                issues.push(join(path, "auth.username"), reason);
            }
        }
        AuthConfig::ClientCertificate {
            cert_path,
            key_path,
        } => {
            check_file(cert_path, &join(path, "auth.cert_path"), issues);
            check_file(key_path, &join(path, "auth.key_path"), issues);
            if environment.author_name().is_empty() {
                issues.push(
                    join(path, "author"),
                    "must be set for client-certificate authentication, which has no \
                     username to record as the author",
                );
            }
        }
    }
}

/// Why a basic-auth username can't work, if it can't. Icinga looks the
/// API user up by exactly this name.
fn username_problem(username: &str) -> Option<&'static str> {
    if username.trim().is_empty() {
        Some("must not be empty")
    } else if username.contains(':') {
        Some(USERNAME_COLON_REASON)
    } else if username.trim() != username {
        Some("must not start or end with whitespace")
    } else {
        None
    }
}

/// Paths to certificates and keys: set, and absolute because relative
/// paths would depend on the directory the app happens to start in.
fn check_file(file: &Path, path: &str, issues: &mut Issues) {
    if file.as_os_str().is_empty() {
        issues.push(path, "must not be empty");
    } else if !file.is_absolute() {
        let hint = if file.starts_with("~") {
            " (`~` is not expanded)"
        } else {
            ""
        };
        issues.push(path, format!("must be an absolute path{hint}"));
    }
}

/// The URLs: at least one, at most [`MAX_API_URLS`], each usable and
/// listed once (the same URL twice would only be tried twice), with its
/// pin and server name.
fn check_urls(urls: &[ApiUrl], path: &str, issues: &mut Issues) {
    if urls.is_empty() {
        issues.push(join(path, "urls"), "must list at least one URL");
    } else if urls.len() > MAX_API_URLS {
        issues.push(
            join(path, "urls"),
            format!("must list at most {MAX_API_URLS} URLs"),
        );
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (index, url) in urls.iter().enumerate() {
        let url_path = join(path, &format!("urls[{index}]"));
        match parse_api_url(&url.url) {
            Ok(parsed) => {
                if let Some(first) = seen.get(parsed.as_str()) {
                    issues.push(
                        join(&url_path, "url"),
                        format!("is already listed as URL {}", first + 1),
                    );
                } else {
                    seen.insert(parsed.to_string(), index);
                }
            }
            Err(reason) => issues.push(join(&url_path, "url"), reason),
        }
        check_pin(url, &url_path, issues);
    }
}

fn check_tls(tls: &TlsConfig, path: &str, issues: &mut Issues) {
    if let Some(ca_file) = &tls.ca_file {
        check_file(ca_file, &join(path, "ca_file"), issues);
    }
}

/// A URL's pinned fingerprint and server name.
fn check_pin(url: &ApiUrl, path: &str, issues: &mut Issues) {
    if let Some(fingerprint) = &url.pinned_sha256
        && let Err(error) = parse_fingerprint(fingerprint)
    {
        let reason = match error {
            ConfigError::InvalidFingerprint(reason) => reason,
            other => other.to_string(),
        };
        issues.push(
            join(path, "pinned_sha256"),
            format!("is not a SHA-256 fingerprint: {reason}"),
        );
    }
    if let Some(server_name) = &url.server_name
        && let Some(reason) = server_name_problem(server_name)
    {
        issues.push(join(path, "server_name"), reason);
    }
}

/// Why a TLS server name can't be used, if it can't. The certificate is
/// checked against a bare DNS name or IP address: no scheme, port,
/// brackets or spaces.
fn server_name_problem(name: &str) -> Option<&'static str> {
    if name.trim().is_empty() {
        return Some("must not be empty when set");
    }
    if name.trim() != name {
        return Some("must not start or end with whitespace");
    }
    if name.parse::<IpAddr>().is_ok() {
        return None;
    }
    if name.contains('/') {
        Some("must be a host name or an IP address, not a URL")
    } else if name.starts_with('[') {
        Some("must be an IP address without brackets")
    } else if name.contains(':') {
        Some("must not contain a port")
    } else if !name.is_ascii() {
        Some("must be a host name in ASCII (international names in their xn-- form)")
    } else if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        Some("must be a host name or an IP address")
    } else {
        None
    }
}

fn check_groups(groups: &[DashboardGroup], path: &str, issues: &mut Issues) {
    let mut group_ids = UniqueIds::default();
    let mut dashboard_ids = UniqueIds::default();
    for (group_index, group) in groups.iter().enumerate() {
        let group_path = join(path, &format!("groups[{group_index}]"));
        group_ids.check(issues, &group_path, &group.id);
        check_name(&group.name, &join(&group_path, "name"), issues);
        for (index, dashboard) in group.dashboards.iter().enumerate() {
            let dashboard_path = format!("{group_path}.dashboards[{index}]");
            dashboard_ids.check(issues, &dashboard_path, &dashboard.id);
            check_dashboard(dashboard, &dashboard_path, issues);
        }
    }
}

fn check_dashboard(dashboard: &Dashboard, path: &str, issues: &mut Issues) {
    check_name(&dashboard.name, &join(path, "name"), issues);
    if dashboard.views.is_empty() {
        issues.push(join(path, "views"), "must list at least one view");
    } else if dashboard.views.len() > MAX_VIEWS {
        issues.push(
            join(path, "views"),
            format!("must list at most {MAX_VIEWS} views"),
        );
    }
    let mut ids = UniqueIds::default();
    for (index, view) in dashboard.views.iter().enumerate() {
        let view_path = join(path, &format!("views[{index}]"));
        ids.check(issues, &view_path, &view.id);
        check_view(view, &view_path, issues);
    }
}

fn check_view(view: &View, path: &str, issues: &mut Issues) {
    if view.object_kind == ObjectKind::Hosts && view.list_grouping() == GroupBy::ServiceGroup {
        issues.push(
            join(path, "group_by"),
            "hosts can't be grouped by service group",
        );
    }
    let grouped = matches!(
        view.display,
        ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles
    );
    if grouped
        && view.groups.by == GroupSource::CustomVar
        && view.groups.custom_var_name().is_empty()
    {
        issues.push(
            join(path, "groups.custom_var"),
            "must name a host custom variable to group by",
        );
    }
    if grouped
        && let Some(index) = view
            .groups
            .host_groups
            .iter()
            .position(|group| group.trim().is_empty())
    {
        issues.push(
            join(path, &format!("groups.host_groups[{index}]")),
            "must not be empty",
        );
    }
    if view.display == ViewDisplay::EventStream && !STREAM_LINES.contains(&view.stream.lines) {
        issues.push(
            join(path, "stream.lines"),
            format!(
                "must be between {} and {}",
                STREAM_LINES.start(),
                STREAM_LINES.end()
            ),
        );
    }
}

fn check_notifications(settings: &NotificationSettings, path: &str, issues: &mut Issues) {
    check_quiet_hours(&settings.quiet_hours, &join(path, "quiet_hours"), issues);
    let mut seen: HashMap<&ObjectKey, usize> = HashMap::new();
    for (index, entry) in settings.objects.iter().enumerate() {
        let entry_path = join(path, &format!("objects[{index}]"));
        check_object_key(&entry.object, &join(&entry_path, "object"), issues);
        if let Some(until) = entry.until
            && !until.as_unix_seconds().is_finite()
        {
            issues.push(join(&entry_path, "until"), "must be a valid time");
        }
        if let Some(first) = seen.get(&entry.object) {
            issues.push(
                entry_path,
                format!(
                    "`{}` already has an override at objects[{first}]",
                    excerpt(&entry.object.to_string(), MAX_VALUE_CHARS)
                ),
            );
        } else {
            seen.insert(&entry.object, index);
        }
    }
}

fn check_quiet_hours(quiet_hours: &QuietHours, path: &str, issues: &mut Issues) {
    for (field, minute) in [
        ("start_minute", quiet_hours.start_minute),
        ("end_minute", quiet_hours.end_minute),
    ] {
        if minute >= MINUTES_PER_DAY {
            issues.push(
                join(path, field),
                format!("must be below {MINUTES_PER_DAY} (minutes after midnight)"),
            );
        }
    }
}

fn check_object_key(object: &ObjectKey, path: &str, issues: &mut Issues) {
    if object.host_name().as_str().trim().is_empty() {
        issues.push(path, "the host name must not be empty");
    }
    if let Some(service) = object.as_service()
        && service.name.trim().is_empty()
    {
        issues.push(path, "the service name must not be empty");
    }
}
