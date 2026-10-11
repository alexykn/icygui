//! API users: Basic auth and permission checks (`apiuser.cpp`,
//! `FilterUtility::HasPermission`).

use base64::Engine as _;

use ic_model::glob_matches;

use crate::config::MockUser;

/// An authenticated API user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Principal {
    pub(crate) name: String,
    /// Permissions as configured (for `GET /v1`).
    pub(crate) permissions: Vec<String>,
}

impl Principal {
    pub(crate) fn new(user: &MockUser) -> Self {
        Self {
            name: user.username.clone(),
            permissions: user.permissions.clone(),
        }
    }

    /// Icinga's wildcard match of the required permission against every
    /// configured one, case-insensitively.
    pub(crate) fn has_permission(&self, required: &str) -> bool {
        if required.is_empty() {
            return true;
        }
        self.permissions.iter().any(|p| glob_matches(p, required))
    }
}

/// The configured users.
#[derive(Clone, Debug, Default)]
pub(crate) struct Users {
    users: Vec<MockUser>,
}

impl Users {
    pub(crate) fn new(users: Vec<MockUser>) -> Self {
        Self { users }
    }

    /// `ApiUser::GetByAuthHeader`: `Basic base64(user:password)`. Unknown
    /// users, empty passwords and wrong passwords fail.
    pub(crate) fn authenticate(&self, header: Option<&str>) -> Option<Principal> {
        let header = header?;
        let (scheme, encoded) = header.split_once(' ')?;
        if scheme != "Basic" {
            return None;
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded.trim())
            .ok()?;
        let credentials = String::from_utf8(decoded).ok()?;
        let (username, password) = credentials.split_once(':')?;
        if password.is_empty() {
            return None;
        }
        let user = self.users.iter().find(|u| u.username == username)?;
        constant_time_eq(password.as_bytes(), user.password.as_bytes())
            .then(|| Principal::new(user))
    }

    /// `ApiUser::GetByClientCN`.
    pub(crate) fn by_client_cn(&self, cn: &str) -> Option<Principal> {
        self.users
            .iter()
            .find(|u| u.client_cn.as_deref() == Some(cn))
            .map(Principal::new)
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(credentials: &str) -> String {
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(credentials)
        )
    }

    #[test]
    fn basic_auth() {
        let users = Users::new(vec![
            MockUser::root(),
            MockUser::new("ro", "pw:with:colons", &[]),
        ]);
        assert_eq!(
            users
                .authenticate(Some(&header("root:icinga")))
                .unwrap()
                .name,
            "root"
        );
        assert_eq!(
            users
                .authenticate(Some(&header("ro:pw:with:colons")))
                .unwrap()
                .name,
            "ro"
        );
        assert!(users.authenticate(Some(&header("root:wrong"))).is_none());
        assert!(users.authenticate(Some(&header("root:"))).is_none());
        assert!(users.authenticate(Some(&header("nobody:icinga"))).is_none());
        assert!(users.authenticate(Some("Bearer abc")).is_none());
        assert!(users.authenticate(Some("Basic !!!")).is_none());
        assert!(users.authenticate(None).is_none());
    }

    #[test]
    fn permissions_follow_icingas_match() {
        let user = Principal::new(&MockUser::new("u", "p", &[r"literal\*", "Events/??"]));
        assert!(user.has_permission("literal*"), "escaped star");
        assert!(!user.has_permission("literalx"));
        assert!(user.has_permission("EVENTS/ab"));
        assert!(!user.has_permission("events/abc"));
    }

    #[test]
    fn permissions_use_wildcards_case_insensitively() {
        let user = Principal::new(&MockUser::new(
            "u",
            "p",
            &["objects/query/Host", "actions/*", "events/CheckResult"],
        ));
        assert!(user.has_permission("objects/query/Host"));
        assert!(user.has_permission("objects/query/host"));
        assert!(!user.has_permission("objects/query/Service"));
        assert!(user.has_permission("actions/acknowledge-problem"));
        assert!(user.has_permission("events/checkresult"));
        assert!(!user.has_permission("events/StateChange"));
        assert!(!user.has_permission("filter-expression"));
        let root = Principal::new(&MockUser::root());
        assert!(root.has_permission("filter-expression"));
        assert!(root.has_permission("anything/at/all"));
    }
}
