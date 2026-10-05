//! Lookups on the whole configuration, and the ids that tie environments,
//! groups and dashboards to the keychain and to `ic-rules`' scope references.

use std::collections::HashSet;

use uuid::Uuid;

use crate::model::{Config, Environment};

/// A fresh random id (a UUID v4 in its hyphenated, lowercase form).
pub fn new_id() -> String {
    Uuid::new_v4().to_string()
}

impl Config {
    /// The environment with this id.
    pub fn environment(&self, id: &str) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| environment.id == id)
    }

    /// The environment with this id, for changing it.
    pub fn environment_mut(&mut self, id: &str) -> Option<&mut Environment> {
        self.environments
            .iter_mut()
            .find(|environment| environment.id == id)
    }

    /// Gives a fresh id to every environment, group and dashboard whose id
    /// is blank or already taken by an earlier one, and returns how many
    /// ids changed. Hand-written files often have no ids at all.
    ///
    /// Environment ids must be unique in the config; group and dashboard
    /// ids within their environment. The first holder of a duplicated id
    /// keeps it.
    pub fn repair_ids(&mut self) -> usize {
        let mut changed = 0;
        let mut environment_ids = HashSet::new();
        for environment in &mut self.environments {
            changed += claim_id(&mut environment.id, &mut environment_ids);
            let mut group_ids = HashSet::new();
            let mut dashboard_ids = HashSet::new();
            for group in &mut environment.groups {
                changed += claim_id(&mut group.id, &mut group_ids);
                for dashboard in &mut group.dashboards {
                    changed += claim_id(&mut dashboard.id, &mut dashboard_ids);
                }
            }
        }
        changed
    }
}

/// Keeps `id` if it is usable and not in `taken`, otherwise replaces it
/// with a fresh one. Returns 1 if it was replaced.
fn claim_id(id: &mut String, taken: &mut HashSet<String>) -> usize {
    if !id.trim().is_empty() && taken.insert(id.clone()) {
        return 0;
    }
    *id = new_id();
    taken.insert(id.clone());
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthConfig, DashboardGroup};

    fn environment(id: &str) -> Environment {
        let mut environment = Environment::new(
            "prod",
            "https://m:5665",
            AuthConfig::Basic {
                username: "u".to_owned(),
            },
        );
        environment.id = id.to_owned();
        environment
    }

    #[test]
    fn new_ids_are_hyphenated_v4_uuids() {
        let id = new_id();
        let parsed = Uuid::parse_str(&id).unwrap();
        assert_eq!(parsed.get_version_num(), 4);
        assert_eq!(id, parsed.hyphenated().to_string());
        assert_ne!(new_id(), new_id());
    }

    #[test]
    fn finds_environments_by_id() {
        let mut config = Config {
            environments: vec![environment("a"), environment("b")],
            ..Config::default()
        };
        assert_eq!(config.environment("b").map(|e| e.id.as_str()), Some("b"));
        assert!(config.environment("c").is_none());
        config.environment_mut("a").unwrap().name = "renamed".to_owned();
        assert_eq!(config.environments[0].name, "renamed");
        assert!(config.environment_mut("c").is_none());
    }

    #[test]
    fn repair_keeps_good_ids() {
        let mut config = Config {
            environments: vec![environment("a"), environment("b")],
            ..Config::default()
        };
        let before = config.clone();
        assert_eq!(config.repair_ids(), 0);
        assert_eq!(config, before);
    }

    #[test]
    fn repair_replaces_blank_and_duplicate_ids() {
        let mut first = environment("same");
        first.groups[0].id = String::new();
        first.groups[0].dashboards[1].id = first.groups[0].dashboards[0].id.clone();
        let mut extra = DashboardGroup::new("extra");
        extra.id = "  ".to_owned();
        first.groups.push(extra);
        let second = environment("same");
        let third = environment("");
        let mut config = Config {
            environments: vec![first, second, third],
            ..Config::default()
        };
        let original_dashboard = config.environments[0].groups[0].dashboards[0].id.clone();

        // Environments 2 and 3, groups 1 and 2 of environment 1, dashboard 2.
        assert_eq!(config.repair_ids(), 5);
        assert_eq!(config.environments[0].id, "same");
        assert_eq!(
            config.environments[0].groups[0].dashboards[0].id,
            original_dashboard
        );
        let environment_ids: HashSet<&str> =
            config.environments.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(environment_ids.len(), 3);
        for environment in &config.environments {
            let group_ids: HashSet<&str> =
                environment.groups.iter().map(|g| g.id.as_str()).collect();
            assert_eq!(group_ids.len(), environment.groups.len());
            assert!(group_ids.iter().all(|id| !id.trim().is_empty()));
            let dashboards: Vec<&str> = environment
                .groups
                .iter()
                .flat_map(|g| g.dashboards.iter().map(|d| d.id.as_str()))
                .collect();
            let unique: HashSet<&str> = dashboards.iter().copied().collect();
            assert_eq!(unique.len(), dashboards.len());
        }
        assert_eq!(config.repair_ids(), 0, "repairing twice changes nothing");
    }

    #[test]
    fn group_and_dashboard_ids_may_repeat_across_environments() {
        let first = environment("a");
        let mut second = environment("b");
        second.groups = first.groups.clone();
        let mut config = Config {
            environments: vec![first, second],
            ..Config::default()
        };
        assert_eq!(config.repair_ids(), 0);
    }
}
