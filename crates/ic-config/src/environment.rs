//! Constructors and accessors for environments, dashboard groups and
//! dashboards, and the dashboards a new environment starts with.

use ic_rules::{NotificationSettings, ScopeSetting};
use url::Url;

use crate::config::new_id;
use crate::error::ConfigError;
use crate::fingerprint::parse_fingerprint;
use crate::model::{
    AuthConfig, Dashboard, DashboardGroup, Environment, ObjectKind, TlsConfig, View,
};

impl Environment {
    /// A new environment with a fresh id, the [default dashboards]
    /// and default notification settings. `name` and `url` are stored
    /// trimmed.
    ///
    /// It trusts the operating system's root certificates, so a server
    /// with a publicly trusted certificate works right away. For Icinga's
    /// own CA, set [`TlsConfig::ca_file`] or pin the certificate
    /// ([`TlsConfig::pinned_sha256`]).
    ///
    /// [default dashboards]: default_groups
    pub fn new(name: &str, url: &str, auth: AuthConfig) -> Self {
        Self {
            id: new_id(),
            name: name.trim().to_owned(),
            url: url.trim().to_owned(),
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
    /// otherwise the basic-auth username.
    ///
    /// Empty for client-certificate authentication without an author;
    /// [`Config::validate`](crate::Config::validate) reports that case.
    pub fn author_name(&self) -> &str {
        match self.author.as_deref().map(str::trim) {
            Some(author) if !author.is_empty() => author,
            _ => match &self.auth {
                AuthConfig::Basic { username } => username,
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

    /// The API base URL, checked the way [`Config::validate`] checks it and
    /// with a path that ends in `/`, so `url.join("v1/status")` keeps any
    /// path prefix (a reverse proxy's `/icinga/`).
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidUrl`] when the URL is empty, malformed, not
    /// `https`, has no host, contains credentials, a query or a fragment,
    /// or already ends in `/v1`.
    ///
    /// [`Config::validate`]: crate::Config::validate
    pub fn api_url(&self) -> Result<Url, ConfigError> {
        parse_api_url(&self.url).map_err(|reason| ConfigError::InvalidUrl {
            url: self.url.clone(),
            reason,
        })
    }
}

impl TlsConfig {
    /// The pinned certificate fingerprint as bytes, if one is set.
    ///
    /// # Errors
    ///
    /// [`ConfigError::InvalidFingerprint`] when
    /// [`TlsConfig::pinned_sha256`] is set but not a valid fingerprint (see
    /// [`parse_fingerprint`]).
    pub fn pinned_fingerprint(&self) -> Result<Option<[u8; 32]>, ConfigError> {
        self.pinned_sha256
            .as_deref()
            .map(parse_fingerprint)
            .transpose()
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

/// Checks an environment URL; the error is a reason for the settings UI.
pub(crate) fn parse_api_url(text: &str) -> Result<Url, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("must not be empty".to_owned());
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
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "must not contain a user name or password; set them in the authentication settings"
                .to_owned(),
        );
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("must not contain a query (?…) or fragment (#…)".to_owned());
    }
    if url.path().trim_end_matches('/').ends_with("/v1") {
        return Err("must not end in /v1: icygui adds the API paths itself".to_owned());
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
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
        assert_eq!(first.url, "https://master-01:5665");
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
        assert!(reason("https://user:secret@master-01:5665").contains("user name or password"));
        assert!(reason("https://user@master-01:5665").contains("user name or password"));
        assert!(reason("https://master-01:5665/?x=1").contains("query"));
        assert!(reason("https://master-01:5665/#top").contains("fragment"));
        assert!(reason("https://master-01:5665/v1").contains("/v1"));
        assert!(reason("https://master-01:5665/v1/").contains("/v1"));
        assert!(reason("https://exa mple.com").starts_with("is not a valid URL"));
        assert!(reason("https://master-01:99999").starts_with("is not a valid URL"));
    }

    #[test]
    fn api_url_reports_the_configured_url() {
        let mut environment = Environment::new("prod", "http://m:5665", basic("u"));
        match environment.api_url() {
            Err(ConfigError::InvalidUrl { url, reason }) => {
                assert_eq!(url, "http://m:5665");
                assert!(reason.starts_with("must use https"));
            }
            other => panic!("expected an invalid URL, got {other:?}"),
        }
        environment.url = "https://m:5665".to_owned();
        assert_eq!(environment.api_url().unwrap().as_str(), "https://m:5665/");
    }

    #[test]
    fn pinned_fingerprints_parse() {
        let mut tls = TlsConfig::default();
        assert_eq!(tls.pinned_fingerprint().unwrap(), None);
        tls.pinned_sha256 = Some(["0A"; 32].join(":"));
        assert_eq!(tls.pinned_fingerprint().unwrap(), Some([0x0a; 32]));
        tls.pinned_sha256 = Some("0A:0B".to_owned());
        assert!(matches!(
            tls.pinned_fingerprint(),
            Err(ConfigError::InvalidFingerprint(_))
        ));
    }
}
