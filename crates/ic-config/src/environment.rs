//! Constructors and accessors for environments, dashboard groups and
//! dashboards, and the dashboards a new environment starts with.

use std::borrow::Cow;

use ic_rules::{NotificationSettings, ScopeSetting};
use url::Url;

use crate::config::new_id;
use crate::error::{ConfigError, MAX_VALUE_CHARS, excerpt};
use crate::fingerprint::parse_fingerprint;
use crate::model::{
    ApiUrl, AuthConfig, Dashboard, DashboardGroup, Environment, ObjectKind, TlsConfig, View,
};

impl Environment {
    /// A new environment with a fresh id, one URL, the [default
    /// dashboards] and default notification settings. `name` and `url` are
    /// stored trimmed; more URLs go into [`Environment::urls`].
    ///
    /// It trusts the operating system's root certificates, so a server
    /// with a publicly trusted certificate works right away. For Icinga's
    /// own CA, set [`TlsConfig::ca_file`] or pin the certificate
    /// ([`ApiUrl::pinned_sha256`]).
    ///
    /// [default dashboards]: default_groups
    pub fn new(name: &str, url: &str, auth: AuthConfig) -> Self {
        Self {
            id: new_id(),
            name: name.trim().to_owned(),
            urls: vec![ApiUrl::new(url)],
            auth,
            tls: TlsConfig {
                use_system_roots: true,
                ..TlsConfig::default()
            },
            author: None,
            groups: default_groups(),
            notifications: NotificationSettings::default(),
        }
    }

    /// The name recorded as the author of acknowledgements, downtimes and
    /// comments: [`Environment::author`] when it is set and not blank,
    /// otherwise the basic-auth username, both without surrounding
    /// whitespace.
    ///
    /// Empty for client-certificate authentication without an author;
    /// [`Config::validate`](crate::Config::validate) reports that case.
    pub fn author_name(&self) -> &str {
        match self.author.as_deref().map(str::trim) {
            Some(author) if !author.is_empty() => author,
            _ => match &self.auth {
                AuthConfig::Basic { username } => username.trim(),
                AuthConfig::ClientCertificate { .. } => "",
            },
        }
    }

    /// The group with this id.
    pub fn group(&self, group_id: &str) -> Option<&DashboardGroup> {
        self.groups.iter().find(|group| group.id == group_id)
    }

    /// The group with this id, for changing it.
    pub fn group_mut(&mut self, group_id: &str) -> Option<&mut DashboardGroup> {
        self.groups.iter_mut().find(|group| group.id == group_id)
    }

    /// The dashboard `dashboard_id` in the group `group_id`.
    pub fn dashboard(&self, group_id: &str, dashboard_id: &str) -> Option<&Dashboard> {
        self.group(group_id)?.dashboard(dashboard_id)
    }

    /// The dashboard `dashboard_id` in the group `group_id`, for changing it.
    pub fn dashboard_mut(&mut self, group_id: &str, dashboard_id: &str) -> Option<&mut Dashboard> {
        self.group_mut(group_id)?.dashboard_mut(dashboard_id)
    }

    /// The first URL as written (trimmed), for labels; empty without
    /// URLs.
    pub fn primary_url(&self) -> &str {
        self.urls.first().map_or("", |url| url.url.trim())
    }

    /// Whether the connection settings differ from `other`'s: the URLs
    /// (with their pins and server names), the login or the TLS settings.
    /// Names, authors, dashboards and rules don't count.
    pub fn connection_differs(&self, other: &Self) -> bool {
        self.urls != other.urls || self.auth != other.auth || self.tls != other.tls
    }
}

impl ApiUrl {
    /// A URL without a pin or server name, stored trimmed.
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim().to_owned(),
            pinned_sha256: None,
            server_name: None,
        }
    }

    /// The API base URL, checked the way [`Config::validate`] checks it and
    /// with a path that ends in `/`, so `url.join("v1/status")` keeps any
    /// path prefix (a reverse proxy's `/icinga/`).
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidUrl`] when the URL is empty, malformed, not
    /// `https`, has no host, contains credentials, a query or a fragment,
    /// or already contains the API path (`/v1`, `/v1/objects/…`). The URL
    /// in the error has any user name, password, query and fragment masked,
    /// so the error can be logged and shown.
    ///
    /// [`Config::validate`]: crate::Config::validate
    pub fn api_url(&self) -> Result<Url, ConfigError> {
        parse_api_url(&self.url).map_err(|reason| ConfigError::InvalidUrl {
            url: redact_url(&self.url),
            reason,
        })
    }

    /// The pinned certificate fingerprint as bytes, if one is set.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidFingerprint`] when [`ApiUrl::pinned_sha256`]
    /// is set but not a valid fingerprint (see [`parse_fingerprint`]).
    pub fn pinned_fingerprint(&self) -> Result<Option<[u8; 32]>, ConfigError> {
        self.pinned_sha256
            .as_deref()
            .map(parse_fingerprint)
            .transpose()
    }

    /// The server name override, trimmed, unless blank.
    pub fn server_name(&self) -> Option<&str> {
        self.server_name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
    }

    /// A short label for messages and lists: the host and port of a valid
    /// URL (`master-01.example.com:5665`, with a reverse proxy's path:
    /// `proxy.example.com/icinga`), otherwise the text with any
    /// credentials, query and fragment masked.
    pub fn label(&self) -> String {
        match parse_api_url(&self.url) {
            Ok(url) => {
                let host = url.host_str().unwrap_or_default();
                let mut label = match url.port() {
                    Some(port) => format!("{host}:{port}"),
                    None => host.to_owned(),
                };
                let path = url.path().trim_end_matches('/');
                label.push_str(path);
                label
            }
            Err(_) => redact_url(&self.url),
        }
    }
}

impl DashboardGroup {
    /// An empty, expanded group with a fresh id that inherits the
    /// environment's notification setting. `name` is stored trimmed.
    pub fn new(name: &str) -> Self {
        Self {
            id: new_id(),
            name: name.trim().to_owned(),
            collapsed: false,
            notifications: ScopeSetting::Inherit,
            dashboards: Vec::new(),
        }
    }

    /// The dashboard with this id.
    pub fn dashboard(&self, dashboard_id: &str) -> Option<&Dashboard> {
        self.dashboards
            .iter()
            .find(|dashboard| dashboard.id == dashboard_id)
    }

    /// The dashboard with this id, for changing it.
    pub fn dashboard_mut(&mut self, dashboard_id: &str) -> Option<&mut Dashboard> {
        self.dashboards
            .iter_mut()
            .find(|dashboard| dashboard.id == dashboard_id)
    }
}

impl Dashboard {
    /// A dashboard with a fresh id that inherits its group's notification
    /// setting. `name` is stored trimmed.
    pub fn new(name: &str, view: View) -> Self {
        Self {
            id: new_id(),
            name: name.trim().to_owned(),
            view,
            notifications: ScopeSetting::Inherit,
        }
    }
}

/// The dashboards every new environment starts with: one group,
/// `overview`, holding
/// - `problems`: unhandled service problems, worst first;
/// - `host problems`: unhandled host problems, worst first;
/// - `all services`: every service, worst first.
///
/// Each call returns fresh ids.
pub fn default_groups() -> Vec<DashboardGroup> {
    let problems = View {
        object_kind: ObjectKind::Services,
        problems_only: true,
        hide_handled: true,
        ..View::default()
    };
    let host_problems = View {
        object_kind: ObjectKind::Hosts,
        problems_only: true,
        hide_handled: true,
        ..View::default()
    };
    let all_services = View {
        object_kind: ObjectKind::Services,
        problems_only: false,
        hide_handled: false,
        ..View::default()
    };
    let mut overview = DashboardGroup::new("overview");
    overview.dashboards = vec![
        Dashboard::new("problems", problems),
        Dashboard::new("host problems", host_problems),
        Dashboard::new("all services", all_services),
    ];
    vec![overview]
}

/// Why a URL with a user name or password is rejected.
pub(crate) const CREDENTIALS_REASON: &str =
    "must not contain a user name or password; set them in the authentication settings";

/// The URL endpoints of the Icinga 2 API, the segment after `/v1/` (from the
/// API documentation's permission table).
const API_ENDPOINTS: [&str; 10] = [
    "actions",
    "config",
    "console",
    "debug",
    "events",
    "objects",
    "status",
    "templates",
    "types",
    "variables",
];

/// Checks an environment URL; the error is a reason for the settings UI.
pub(crate) fn parse_api_url(text: &str) -> Result<Url, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("must not be empty".to_owned());
    }
    // Before anything else, so a password is reported as such even in text
    // that is no valid URL.
    if has_credentials(text) {
        return Err(CREDENTIALS_REASON.to_owned());
    }
    let mut url = Url::parse(text).map_err(|error| match error {
        url::ParseError::RelativeUrlWithoutBase => {
            "must be a full URL such as https://icinga.example.com:5665".to_owned()
        }
        url::ParseError::EmptyHost => "has no host name".to_owned(),
        other => format!("is not a valid URL ({other})"),
    })?;
    match url.scheme() {
        "https" => {}
        "http" => {
            return Err("must use https: the Icinga 2 API only accepts TLS connections".to_owned());
        }
        _ => return Err("must start with https://".to_owned()),
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err("has no host name".to_owned());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not contain a query (?…) or fragment (#…)".to_owned());
    }
    if has_api_path(&url) {
        return Err("must not include /v1 or an API path: icygui adds them itself".to_owned());
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

/// Whether the URL's path contains Icinga's API path: a `v1` segment (in
/// any case) at the end or followed by an API endpoint, as in URLs copied
/// from the API documentation (`https://master-01:5665/v1/objects/hosts`).
/// Other segments are a reverse proxy's prefix and are kept, even one named
/// `v1` (`https://gateway.example.com/v1/icinga/`).
fn has_api_path(url: &Url) -> bool {
    let segments: Vec<&str> = url
        .path_segments()
        .into_iter()
        .flatten()
        .filter(|segment| !segment.is_empty())
        .collect();
    segments.iter().enumerate().any(|(index, segment)| {
        segment.eq_ignore_ascii_case("v1")
            && segments.get(index + 1).is_none_or(|next| {
                API_ENDPOINTS
                    .iter()
                    .any(|endpoint| next.eq_ignore_ascii_case(endpoint))
            })
    })
}

/// Whether URL text contains a user name or password
/// (`https://root:secret@master-01:5665`). Also catches them in text that
/// is no valid URL, or has no scheme (`root:secret@master-01:5665`), so
/// that no password is ever written to the settings file.
pub(crate) fn has_credentials(text: &str) -> bool {
    let text = url_text(text);
    userinfo(&text).is_some()
        || Url::parse(&text).is_ok_and(|url| !url.username().is_empty() || url.password().is_some())
}

/// URL text made safe to show and log: a user name and password are
/// replaced by `***`, a query and fragment by `?…` and `#…`, and very long
/// text is cut short. Text that isn't a valid URL is masked the same way.
pub(crate) fn redact_url(text: &str) -> String {
    let text = url_text(text);
    let Some(authority) = authority(&text) else {
        return excerpt(&text, MAX_VALUE_CHARS).into_owned();
    };
    let mut redacted = String::with_capacity(text.len());
    redacted.push_str(&text[..authority.start]);
    let host = match userinfo(&text) {
        Some(userinfo) => {
            redacted.push_str("***");
            userinfo.end
        }
        None if has_credentials(&text) => {
            // Credentials only the URL parser finds; hide everything.
            return "<hidden>".to_owned();
        }
        None => authority.start,
    };
    let rest = &text[host..];
    match rest.find(['?', '#']) {
        Some(cut) => {
            redacted.push_str(&rest[..cut]);
            redacted.push_str(if rest[cut..].starts_with('?') {
                "?…"
            } else {
                "#…"
            });
        }
        None => redacted.push_str(rest),
    }
    excerpt(&redacted, MAX_VALUE_CHARS).into_owned()
}

/// URL text as the URL parser reads it: without surrounding whitespace and
/// control characters, and without tabs and line breaks anywhere.
fn url_text(text: &str) -> Cow<'_, str> {
    let text = text.trim_matches(|c: char| c.is_whitespace() || c.is_control());
    if text.contains(['\t', '\n', '\r']) {
        Cow::Owned(
            text.chars()
                .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    }
}

/// Where the authority of URL text starts and ends (byte offsets): after
/// the scheme (if any) and any slashes, up to the path, query or fragment.
/// It holds the user name and password, if any, then the host and port.
fn authority(text: &str) -> Option<std::ops::Range<usize>> {
    let after_scheme = match text.split_once(':') {
        Some((scheme, _)) if is_scheme(scheme) => scheme.len() + 1,
        _ => 0,
    };
    let rest = text.get(after_scheme..)?;
    let start = after_scheme + (rest.len() - rest.trim_start_matches(['/', '\\']).len());
    let end = text
        .get(start..)?
        .find(['/', '\\', '?', '#'])
        .map_or(text.len(), |offset| start + offset);
    Some(start..end)
}

/// Where the user name and password of URL text are (byte offsets,
/// without the `@` that ends them), if it has any. Like the URL parser,
/// the last `@` in the authority ends them.
fn userinfo(text: &str) -> Option<std::ops::Range<usize>> {
    let authority = authority(text)?;
    let at = text.get(authority.clone())?.rfind('@')?;
    Some(authority.start..authority.start + at)
}

/// Whether `text` is a URL scheme: a letter, then letters, digits, `+`,
/// `-` or `.`.
fn is_scheme(text: &str) -> bool {
    let mut chars = text.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn basic(username: &str) -> AuthConfig {
        AuthConfig::Basic {
            username: username.to_owned(),
        }
    }

    fn certificate() -> AuthConfig {
        AuthConfig::ClientCertificate {
            cert_path: PathBuf::from("/etc/icygui/client.pem"),
            key_path: PathBuf::from("/etc/icygui/client.key"),
        }
    }

    #[test]
    fn new_environments_get_fresh_ids_and_default_dashboards() {
        let first = Environment::new(" prod ", " https://master-01:5665 \n", basic("icygui"));
        let second = Environment::new("prod", "https://master-01:5665", basic("icygui"));
        assert_ne!(first.id, second.id);
        assert_eq!(first.name, "prod");
        assert_eq!(first.urls, [ApiUrl::new("https://master-01:5665")]);
        assert_eq!(first.primary_url(), "https://master-01:5665");
        assert!(first.tls.use_system_roots);
        assert_eq!(first.author, None);
        assert_eq!(first.notifications, NotificationSettings::default());
        assert_eq!(first.groups.len(), 1);
        assert_ne!(first.groups[0].id, second.groups[0].id);
    }

    #[test]
    fn default_groups_match_the_contract() {
        let groups = default_groups();
        assert_eq!(groups.len(), 1);
        let overview = &groups[0];
        assert_eq!(overview.name, "overview");
        assert!(!overview.collapsed);
        assert_eq!(overview.notifications, ScopeSetting::Inherit);
        let names: Vec<&str> = overview
            .dashboards
            .iter()
            .map(|d| d.name.as_str())
            .collect();
        assert_eq!(names, ["problems", "host problems", "all services"]);

        let [problems, host_problems, all_services] = &overview.dashboards[..] else {
            panic!("three dashboards");
        };
        assert_eq!(problems.view.object_kind, ObjectKind::Services);
        assert!(problems.view.problems_only && problems.view.hide_handled);
        assert_eq!(host_problems.view.object_kind, ObjectKind::Hosts);
        assert!(host_problems.view.problems_only && host_problems.view.hide_handled);
        assert_eq!(all_services.view.object_kind, ObjectKind::Services);
        assert!(!all_services.view.problems_only && !all_services.view.hide_handled);
        for dashboard in &overview.dashboards {
            assert!(dashboard.view.filter.is_empty());
            assert_eq!(dashboard.view.sort, crate::Sort::default());
            assert_eq!(dashboard.notifications, ScopeSetting::Inherit);
            assert!(!dashboard.id.is_empty());
        }
        let mut ids: Vec<&str> = std::iter::once(overview.id.as_str())
            .chain(overview.dashboards.iter().map(|d| d.id.as_str()))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 4, "every id is distinct");
    }

    #[test]
    fn author_name_prefers_the_author_then_the_username() {
        let mut environment = Environment::new("prod", "https://m:5665", basic("icygui-api"));
        assert_eq!(environment.author_name(), "icygui-api");
        environment.author = Some("m.keller".to_owned());
        assert_eq!(environment.author_name(), "m.keller");
        environment.author = Some("  m.keller \t".to_owned());
        assert_eq!(environment.author_name(), "m.keller");
        environment.author = Some("   ".to_owned());
        assert_eq!(environment.author_name(), "icygui-api");

        let mut environment = Environment::new("prod", "https://m:5665", basic(" icygui-api\t"));
        assert_eq!(environment.author_name(), "icygui-api");
        environment.auth = basic("  ");
        assert_eq!(environment.author_name(), "");

        let mut environment = Environment::new("prod", "https://m:5665", certificate());
        assert_eq!(environment.author_name(), "");
        environment.author = Some("m.keller".to_owned());
        assert_eq!(environment.author_name(), "m.keller");
    }

    #[test]
    fn looks_up_groups_and_dashboards_by_id() {
        let mut environment = Environment::new("prod", "https://m:5665", basic("u"));
        let group_id = environment.groups[0].id.clone();
        let dashboard_id = environment.groups[0].dashboards[1].id.clone();
        assert_eq!(
            environment
                .dashboard(&group_id, &dashboard_id)
                .map(|d| d.name.as_str()),
            Some("host problems")
        );
        assert!(environment.dashboard(&group_id, "missing").is_none());
        assert!(environment.dashboard("missing", &dashboard_id).is_none());
        assert_eq!(
            environment.group(&group_id).map(|g| g.name.as_str()),
            Some("overview")
        );

        environment
            .dashboard_mut(&group_id, &dashboard_id)
            .unwrap()
            .name = "hosts".to_owned();
        assert_eq!(environment.groups[0].dashboards[1].name, "hosts");
        environment.group_mut(&group_id).unwrap().collapsed = true;
        assert!(environment.groups[0].collapsed);
    }

    #[test]
    fn new_groups_and_dashboards_get_fresh_ids() {
        let group = DashboardGroup::new(" databases ");
        assert_eq!(group.name, "databases");
        assert!(group.dashboards.is_empty());
        assert_eq!(group.notifications, ScopeSetting::Inherit);
        let dashboard = Dashboard::new("replication", View::default());
        assert!(!dashboard.id.is_empty());
        assert_ne!(
            dashboard.id,
            Dashboard::new("replication", View::default()).id
        );
        assert_ne!(group.id, DashboardGroup::new("databases").id);
    }

    #[test]
    fn api_urls_get_a_trailing_slash() {
        let check = |text: &str| parse_api_url(text).map(|url| url.to_string());
        assert_eq!(
            check("https://master-01.example.com:5665").as_deref(),
            Ok("https://master-01.example.com:5665/")
        );
        assert_eq!(
            check("  https://master-01:5665/  ").as_deref(),
            Ok("https://master-01:5665/")
        );
        assert_eq!(
            check("https://proxy.example.com/icinga").as_deref(),
            Ok("https://proxy.example.com/icinga/")
        );
        assert_eq!(
            check("https://[::1]:5665").as_deref(),
            Ok("https://[::1]:5665/")
        );
        assert_eq!(
            check("HTTPS://10.0.0.1:5665").as_deref(),
            Ok("https://10.0.0.1:5665/")
        );
    }

    #[test]
    fn rejects_unusable_api_urls() {
        let reason = |text: &str| parse_api_url(text).unwrap_err();
        assert_eq!(reason(""), "must not be empty");
        assert_eq!(reason("  "), "must not be empty");
        assert!(reason("http://master-01:5665").starts_with("must use https"));
        assert!(reason("master-01.example.com").starts_with("must be a full URL"));
        assert_eq!(reason("ftp://master-01"), "must start with https://");
        assert_eq!(reason("master-01:5665"), "must start with https://");
        assert_eq!(reason("https://"), "has no host name");
        assert_eq!(reason("https://:5665"), "has no host name");
        assert_eq!(
            reason("https://user:secret@master-01:5665"),
            CREDENTIALS_REASON
        );
        assert_eq!(reason("https://user@master-01:5665"), CREDENTIALS_REASON);
        assert!(reason("https://master-01:5665/?x=1").contains("query"));
        assert!(reason("https://master-01:5665/#top").contains("fragment"));
        assert!(reason("https://master-01:5665/v1").contains("/v1"));
        assert!(reason("https://master-01:5665/v1/").contains("/v1"));
        assert!(reason("https://exa mple.com").starts_with("is not a valid URL"));
        assert!(reason("https://master-01:99999").starts_with("is not a valid URL"));
    }

    #[test]
    fn credentials_are_reported_before_anything_else() {
        // Even where the text is no usable URL, a password is the problem
        // to report, and the one that keeps the settings from being saved.
        for text in [
            "http://root:icinga@master-01:5665",
            "root:icinga@master-01:5665",
            "https:/root:icinga@master-01:5665",
            "https://root:icinga@exa mple.com",
            "https://ro\tot:icinga@master-01:5665",
            " \u{1}https://root:icinga@master-01:5665",
            "https://root:p@ss@master-01:5665/v1",
            "https://master-01:5665@evil.example.com",
        ] {
            assert_eq!(
                parse_api_url(text).unwrap_err(),
                CREDENTIALS_REASON,
                "{text:?}"
            );
            assert!(has_credentials(text), "{text:?}");
        }
        for text in [
            "https://master-01:5665",
            "master-01:5665",
            "https://[::1]:5665/icinga/",
            "https://master-01:5665/?q=a@b",
            "https://master-01:5665/path/@x",
            "",
        ] {
            assert!(!has_credentials(text), "{text:?}");
        }
    }

    #[test]
    fn api_paths_are_rejected_but_proxy_prefixes_kept() {
        let reason = "must not include /v1 or an API path: icygui adds them itself";
        for text in [
            "https://master-01:5665/v1",
            "https://master-01:5665/V1/",
            "https://master-01:5665/v1/objects/hosts",
            "https://master-01:5665/v1/status/",
            "https://master-01:5665/v1/Events",
            "https://proxy.example.com/icinga/v1",
            "https://proxy.example.com/icinga/v1/actions/acknowledge-problem",
        ] {
            assert_eq!(parse_api_url(text).unwrap_err(), reason, "{text}");
        }
        for (text, base) in [
            (
                "https://gateway.example.com/v1/icinga",
                "https://gateway.example.com/v1/icinga/",
            ),
            (
                "https://proxy.example.com/v1-icinga/",
                "https://proxy.example.com/v1-icinga/",
            ),
            (
                "https://proxy.example.com/icinga/v2",
                "https://proxy.example.com/icinga/v2/",
            ),
        ] {
            let url = parse_api_url(text).unwrap();
            assert_eq!(url.as_str(), base);
            assert_eq!(
                url.join("v1/status").unwrap().as_str(),
                format!("{base}v1/status")
            );
        }
    }

    #[test]
    fn redacted_urls_hide_credentials_queries_and_fragments() {
        for (text, redacted) in [
            ("https://master-01:5665", "https://master-01:5665"),
            (
                "  https://master-01:5665/icinga/ \n",
                "https://master-01:5665/icinga/",
            ),
            (
                "https://root:icinga@master-01:5665",
                "https://***@master-01:5665",
            ),
            (
                "https://root@master-01:5665/x",
                "https://***@master-01:5665/x",
            ),
            ("https://a:b@c@master-01", "https://***@master-01"),
            ("root:icinga@master-01:5665", "root:***@master-01:5665"),
            (
                "https://root:icinga@exa mple.com",
                "https://***@exa mple.com",
            ),
            ("https://ro\tot:pw@master-01", "https://***@master-01"),
            ("https://master-01/?api_key=s3cret", "https://master-01/?…"),
            ("https://master-01/#token=s3cret", "https://master-01/#…"),
            ("https://u:p@master-01/?k=v#f", "https://***@master-01/?…"),
            ("not a url", "not a url"),
            ("", ""),
        ] {
            assert_eq!(redact_url(text), redacted, "{text:?}");
        }
        let long = format!("https://master-01/{}", "a".repeat(10_000));
        let redacted = redact_url(&long);
        assert_eq!(redacted.chars().count(), MAX_VALUE_CHARS + 1);
        assert!(redacted.ends_with('…'));
    }

    #[test]
    fn invalid_url_errors_never_show_the_password() {
        for url in [
            "https://icygui:hunter2@master-01:5665",
            "http://icygui:hunter2@master-01:5665",
            "icygui:hunter2@master-01:5665",
            "https://icygui:hunter2@master-01:56 65",
            "https://master-01:5665/?password=hunter2",
            "https://master-01:5665/#hunter2",
        ] {
            let environment = Environment::new("prod", url, basic("icygui"));
            let error = environment.urls[0].api_url().unwrap_err();
            for text in [error.to_string(), format!("{error:?}")] {
                assert!(!text.contains("hunter2"), "{text}");
                assert!(text.contains("master-01"), "{text}");
            }
        }
    }

    #[test]
    fn api_url_reports_the_configured_url() {
        let mut environment = Environment::new("prod", "http://m:5665", basic("u"));
        match environment.urls[0].api_url() {
            Err(ConfigError::InvalidUrl { url, reason }) => {
                assert_eq!(url, "http://m:5665");
                assert!(reason.starts_with("must use https"));
            }
            other => panic!("expected an invalid URL, got {other:?}"),
        }
        environment.urls[0].url = "https://m:5665".to_owned();
        assert_eq!(
            environment.urls[0].api_url().unwrap().as_str(),
            "https://m:5665/"
        );
    }

    #[test]
    fn url_labels_are_short_and_never_show_credentials() {
        for (text, label) in [
            (
                "https://master-01.example.com:5665",
                "master-01.example.com:5665",
            ),
            ("https://master-01.example.com", "master-01.example.com"),
            (
                "https://proxy.example.com/icinga/",
                "proxy.example.com/icinga",
            ),
            ("https://[::1]:5665", "[::1]:5665"),
            (
                "https://root:pw@master-01:5665",
                "https://***@master-01:5665",
            ),
            ("not a url", "not a url"),
        ] {
            assert_eq!(ApiUrl::new(text).label(), label, "{text}");
        }
    }

    #[test]
    fn server_names_are_trimmed_and_blank_ones_unset() {
        let mut url = ApiUrl::new("https://10.0.0.5:5665");
        assert_eq!(url.server_name(), None);
        url.server_name = Some("  ".to_owned());
        assert_eq!(url.server_name(), None);
        url.server_name = Some(" master-01 ".to_owned());
        assert_eq!(url.server_name(), Some("master-01"));
    }

    #[test]
    fn connection_changes_are_told_from_other_changes() {
        let original = Environment::new("prod", "https://master-01:5665", basic("u"));
        let mut renamed = original.clone();
        renamed.name = "production".to_owned();
        renamed.author = Some("m.keller".to_owned());
        renamed.groups.clear();
        assert!(!original.connection_differs(&renamed));
        let mut second = original.clone();
        second.urls.push(ApiUrl::new("https://master-02:5665"));
        assert!(original.connection_differs(&second));
        let mut pinned = original.clone();
        pinned.urls[0].pinned_sha256 = Some("AB".repeat(32));
        assert!(original.connection_differs(&pinned));
        let mut tls = original.clone();
        tls.tls.use_system_roots = false;
        assert!(original.connection_differs(&tls));
        let mut login = original.clone();
        login.auth = basic("other");
        assert!(original.connection_differs(&login));
    }

    #[test]
    fn pinned_fingerprints_parse() {
        let mut url = ApiUrl::new("https://m:5665");
        assert_eq!(url.pinned_fingerprint().unwrap(), None);
        url.pinned_sha256 = Some(["0A"; 32].join(":"));
        assert_eq!(url.pinned_fingerprint().unwrap(), Some([0x0a; 32]));
        url.pinned_sha256 = Some("0A:0B".to_owned());
        assert!(matches!(
            url.pinned_fingerprint(),
            Err(ConfigError::InvalidFingerprint(_))
        ));
    }
}
