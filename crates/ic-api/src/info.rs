//! `GET /v1`: who we are and what we may do.

use ic_model::glob_matches;

/// The authenticated API user, its permissions and the Icinga version.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApiInfo {
    /// The `ApiUser` name.
    pub user: String,
    /// Permission patterns as Icinga lists them (`objects/query/*`,
    /// `actions/acknowledge-problem (filtered)`).
    pub permissions: Vec<String>,
    /// Icinga version (`v2.15.6`).
    pub version: String,
}

/// The suffix `GET /v1` appends to permissions restricted by a filter.
const FILTERED_SUFFIX: &str = " (filtered)";

impl ApiInfo {
    /// Whether the user holds `permission` (for example
    /// `actions/acknowledge-problem` or `objects/query/Host`).
    ///
    /// Follows Icinga's `FilterUtility::HasPermission`: both sides are
    /// compared case-insensitively and each granted entry is a glob
    /// pattern (`*` matches any run of characters including `/`, `?` one
    /// character), so `*`, `actions/*` and `objects/query/*` work. Entries
    /// restricted by a filter (`… (filtered)`) count as allowed: the
    /// filter limits which objects, not whether the endpoint can be used.
    #[must_use]
    pub fn allows(&self, permission: &str) -> bool {
        if permission.is_empty() {
            return true;
        }
        self.permissions.iter().any(|granted| {
            let pattern = granted
                .strip_suffix(FILTERED_SUFFIX)
                .unwrap_or(granted)
                .trim();
            glob_matches(pattern, permission)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(permissions: &[&str]) -> ApiInfo {
        ApiInfo {
            user: "icygui".to_owned(),
            permissions: permissions.iter().map(|p| (*p).to_owned()).collect(),
            version: "v2.15.6".to_owned(),
        }
    }

    #[test]
    fn star_allows_everything() {
        let all = info(&["*"]);
        assert!(all.allows("actions/acknowledge-problem"));
        assert!(all.allows("objects/query/Host"));
        assert!(all.allows("events/StateChange"));
    }

    #[test]
    fn hierarchical_wildcards() {
        let user = info(&["objects/query/*", "status/query", "events/*", "actions/*"]);
        assert!(user.allows("objects/query/Host"));
        assert!(user.allows("objects/query/Service"));
        assert!(user.allows("status/query"));
        assert!(user.allows("events/CheckResult"));
        assert!(user.allows("actions/schedule-downtime"));
        assert!(!user.allows("objects/modify/Host"));
        assert!(!user.allows("config/modify"));
        assert!(!user.allows("console"));
    }

    #[test]
    fn exact_permissions_are_case_insensitive() {
        let viewer = info(&["objects/query/Host", "events/StateChange"]);
        assert!(viewer.allows("objects/query/host"));
        assert!(viewer.allows("OBJECTS/QUERY/HOST"));
        assert!(viewer.allows("events/statechange"));
        assert!(!viewer.allows("objects/query/Service"));
        assert!(
            !viewer.allows("objects/query/Hostgroup"),
            "no prefix matching"
        );
        assert!(!viewer.allows("actions/acknowledge-problem"));
    }

    #[test]
    fn filtered_permissions_count_as_allowed() {
        let user = info(&["objects/query/Host (filtered)", "actions/* (filtered)"]);
        assert!(user.allows("objects/query/Host"));
        assert!(user.allows("actions/remove-comment"));
        assert!(!user.allows("objects/query/Service"));
    }

    #[test]
    fn no_permissions_allow_nothing() {
        assert!(!info(&[]).allows("status/query"));
        assert!(info(&[]).allows(""), "an empty requirement is always met");
    }

    #[test]
    fn escapes_and_case_are_icingas() {
        // The shared matcher's rules (ic_model::glob_matches).
        let user = info(&[r"literal\*", "Actions/*"]);
        assert!(user.allows("literal*"));
        assert!(!user.allows("literalx"));
        assert!(user.allows("ACTIONS/remove-comment"));
    }
}
