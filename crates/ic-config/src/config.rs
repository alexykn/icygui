//! Lookups on the whole configuration, and the ids that tie environments,
//! groups and dashboards to the keychain and to `ic-rules`' scope references.

use std::collections::HashSet;

use uuid::Uuid;

use crate::model::{Config, Environment};

/// The namespace of ids derived from settings content (UUID v5).
///
/// Never change it, or how [`derived_id`] encodes its input: a settings
/// file that can't be written (read-only, managed by Nix or Ansible) gets
/// its missing ids derived again at every start, and environment ids are
/// the keychain accounts its passwords are stored under.
const DERIVED_ID_NAMESPACE: Uuid = Uuid::from_u128(0xae9b_66de_04ac_458f_8182_9f8a_b51e_2af9);

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

    /// Gives a new id to every environment, group and dashboard whose id
    /// is blank or already taken by an earlier one, and returns how many
    /// ids changed. Hand-written files often have no ids at all.
    ///
    /// Environment ids must be unique in the config; group and dashboard
    /// ids within their environment. The first holder of a duplicated id
    /// keeps it, and new ids never take one that an entry holds.
    ///
    /// The new ids are derived from the entries' content (UUID v5), not
    /// random, so the same file always gets the same ids, even when it
    /// can't be saved: an environment's from its name and URL, a group's
    /// from its environment's id and its name, a dashboard's from its
    /// environment's and group's ids and its name. Entries that are the
    /// same in all of these get different ids in file order. A derived id
    /// changes only when its entry's name or URL does, or an identical
    /// entry before it comes or goes; and a password stored under a derived
    /// environment id is never offered to a server at another URL.
    pub fn repair_ids(&mut self) -> usize {
        let mut changed = 0;
        let mut environment_ids = Scope::new(self.environments.iter().map(|e| e.id.as_str()));
        for (index, environment) in self.environments.iter_mut().enumerate() {
            if !environment_ids.keeps(index) {
                environment.id = environment_ids.derive(&[
                    "environment",
                    environment.name.trim(),
                    environment.url.trim(),
                ]);
                changed += 1;
            }
        }
        for environment in &mut self.environments {
            changed += repair_group_and_dashboard_ids(environment);
        }
        changed
    }
}

/// [`Config::repair_ids`] for the groups and dashboards of one environment,
/// whose id must already be repaired.
fn repair_group_and_dashboard_ids(environment: &mut Environment) -> usize {
    let mut changed = 0;
    let mut group_ids = Scope::new(environment.groups.iter().map(|group| group.id.as_str()));
    for (index, group) in environment.groups.iter_mut().enumerate() {
        if !group_ids.keeps(index) {
            group.id = group_ids.derive(&["group", &environment.id, group.name.trim()]);
            changed += 1;
        }
    }
    let mut dashboard_ids = Scope::new(
        environment
            .groups
            .iter()
            .flat_map(|group| group.dashboards.iter())
            .map(|dashboard| dashboard.id.as_str()),
    );
    let mut index = 0;
    for group in &mut environment.groups {
        for dashboard in &mut group.dashboards {
            if !dashboard_ids.keeps(index) {
                dashboard.id = dashboard_ids.derive(&[
                    "dashboard",
                    &environment.id,
                    &group.id,
                    dashboard.name.trim(),
                ]);
                changed += 1;
            }
            index += 1;
        }
    }
    changed
}

/// The ids of one scope (all environments, or the groups or dashboards of
/// one environment) while they are repaired.
struct Scope {
    /// Whether each entry, in order, keeps its id: it is not blank, and no
    /// earlier entry holds it.
    keeps: Vec<bool>,
    /// Ids in use: the kept ones, and those handed out so far.
    taken: HashSet<String>,
}

impl Scope {
    fn new<'a>(ids: impl Iterator<Item = &'a str>) -> Self {
        let mut taken = HashSet::new();
        let keeps = ids
            .map(|id| !id.trim().is_empty() && taken.insert(id.to_owned()))
            .collect();
        Self { keeps, taken }
    }

    fn keeps(&self, index: usize) -> bool {
        self.keeps.get(index).copied().unwrap_or(true)
    }

    /// The first id derived from `seed` that isn't taken, now taken.
    fn derive(&mut self, seed: &[&str]) -> String {
        // One more attempt than there are taken ids always finds a free
        // one; the random fallback is only for SHA-1 collisions.
        let attempts = u64::try_from(self.taken.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let id = (0..attempts)
            .map(|attempt| derived_id(seed, attempt))
            .find(|id| !self.taken.contains(id))
            .unwrap_or_else(new_id);
        self.taken.insert(id.clone());
        id
    }
}

/// A UUID v5 in [`DERIVED_ID_NAMESPACE`] for `seed` and `attempt`, in the
/// form of [`new_id`]. Each field is prefixed with its length, so
/// different seeds never encode alike.
fn derived_id(seed: &[&str], attempt: u64) -> String {
    let mut name = Vec::new();
    for field in seed {
        let length = u64::try_from(field.len()).unwrap_or(u64::MAX);
        name.extend_from_slice(&length.to_be_bytes());
        name.extend_from_slice(field.as_bytes());
    }
    name.extend_from_slice(&attempt.to_be_bytes());
    Uuid::new_v5(&DERIVED_ID_NAMESPACE, &name).to_string()
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

    /// Two copies of an environment, a group and a dashboard: everything
    /// without ids, as in a hand-written file.
    fn without_ids() -> Config {
        let mut prod = environment("");
        prod.groups.push(prod.groups[0].clone());
        for group in &mut prod.groups {
            group.id.clear();
            for dashboard in &mut group.dashboards {
                dashboard.id.clear();
            }
        }
        Config {
            environments: vec![prod.clone(), prod],
            ..Config::default()
        }
    }

    fn all_ids(config: &Config) -> Vec<String> {
        config
            .environments
            .iter()
            .flat_map(|environment| {
                std::iter::once(environment.id.clone()).chain(environment.groups.iter().flat_map(
                    |group| {
                        std::iter::once(group.id.clone())
                            .chain(group.dashboards.iter().map(|d| d.id.clone()))
                    },
                ))
            })
            .collect()
    }

    #[test]
    fn repaired_ids_are_the_same_every_time() {
        let mut first = without_ids();
        let mut second = without_ids();
        // Two environments, each with two groups of three dashboards.
        assert_eq!(first.repair_ids(), 2 + 2 * (2 + 2 * 3));
        assert_eq!(second.repair_ids(), 18);
        assert_eq!(first.repair_ids(), 0, "repairing twice changes nothing");
        assert_eq!(all_ids(&first), all_ids(&second));

        // Identical entries still get distinct ids, valid UUIDs.
        let ids = all_ids(&first);
        assert_eq!(first.environments[0].id.len(), 36);
        assert_ne!(first.environments[0].id, first.environments[1].id);
        for environment in &first.environments {
            let groups: HashSet<&str> = environment.groups.iter().map(|g| g.id.as_str()).collect();
            assert_eq!(groups.len(), 2);
            let dashboards: HashSet<&str> = environment
                .groups
                .iter()
                .flat_map(|g| g.dashboards.iter().map(|d| d.id.as_str()))
                .collect();
            assert_eq!(dashboards.len(), 6);
        }
        for id in &ids {
            assert_eq!(Uuid::parse_str(id).unwrap().get_version_num(), 5);
        }
    }

    #[test]
    fn repaired_ids_follow_names_and_urls() {
        let mut original = without_ids();
        original.repair_ids();
        // Reordering keeps each entry's id; renaming changes it.
        let mut reordered = without_ids();
        reordered.environments[1].name = "staging".to_owned();
        reordered.environments.swap(0, 1);
        reordered.repair_ids();
        assert_eq!(reordered.environments[1].id, original.environments[0].id);
        assert_ne!(reordered.environments[0].id, original.environments[1].id);
        // Another URL never gets the id, and so the password, of this one.
        let mut moved = without_ids();
        moved.environments[0].url = "https://elsewhere:5665".to_owned();
        moved.repair_ids();
        let before = [&original.environments[0].id, &original.environments[1].id];
        assert!(!before.contains(&&moved.environments[0].id));
        // The unchanged copy is now the first of its kind.
        assert_eq!(moved.environments[1].id, original.environments[0].id);
    }

    #[test]
    fn repaired_ids_never_take_an_id_that_is_in_use() {
        // The second environment already holds the id the first would get.
        let mut probe = without_ids();
        probe.environments.truncate(1);
        probe.repair_ids();
        let derived = probe.environments[0].id.clone();

        let mut config = without_ids();
        config.environments[1].id.clone_from(&derived);
        config.environments[1].name = "other".to_owned();
        config.repair_ids();
        assert_eq!(config.environments[1].id, derived, "the holder keeps it");
        assert_ne!(config.environments[0].id, derived);
        assert!(!config.environments[0].id.is_empty());
    }

    #[test]
    fn derived_ids_never_change() {
        // Keychain accounts depend on these staying the same in every
        // release; see DERIVED_ID_NAMESPACE.
        assert_eq!(
            derived_id(&["environment", "prod", "https://master-01:5665"], 0),
            "5ef87726-6215-59dd-9c32-9868a959e4d5"
        );
        assert_eq!(
            derived_id(&["environment", "prod", "https://master-01:5665"], 1),
            "99963ced-1f96-58f6-8cd6-96bb29a1ac13"
        );
        // Length prefixes keep the fields apart.
        assert_ne!(derived_id(&["ab", "c"], 0), derived_id(&["a", "bc"], 0));
    }
}
