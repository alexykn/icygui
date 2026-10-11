//! Views by group: the host-group grid (topic 05) and summary tiles (topic
//! 04). Both file hosts under host groups (all, or the ones picked by name
//! or glob pattern) or under the values of a host custom variable.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ic_config::{GridColour, GroupOrder, GroupSource, View, ViewDisplay};
use ic_model::{CheckableState, Glob, Host, HostName, HostState, ObjectKey, ServiceState};

use super::board::Board;
use super::{Data, service_range};
use crate::snapshot::{Grid, GridCell, GridGroup, Summary, Tile};
use crate::summary::Tally;

/// The groups a grid or tiles view shows, and which of them a host is in.
#[derive(Debug, Default)]
pub(super) struct Picker {
    /// The view shows groups (a grid or tiles); otherwise every host is
    /// "in" and has no groups.
    active: bool,
    by: GroupSource,
    /// Host groups by name or glob; empty: every host group.
    patterns: Vec<Pattern>,
    /// The host custom variable, without prefix.
    var: String,
    /// A host in several groups is in each (else only in its first).
    each: bool,
}

/// A host group from the view: a name picked as a chip matches exactly
/// that group (Icinga object names are case-sensitive, so `linux` and
/// `Linux` can both exist), a pattern with `*` or `?` matches a family of
/// them like Icinga's `match()` (`ic_model::Glob`, ASCII case ignored).
#[derive(Debug)]
enum Pattern {
    Exact(String),
    Glob(Glob),
}

impl Pattern {
    fn new(text: &str) -> Self {
        let text = text.trim();
        if text.contains(['*', '?']) {
            Self::Glob(Glob::new(text))
        } else {
            Self::Exact(text.to_owned())
        }
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == name,
            Self::Glob(glob) => glob.is_match(name),
        }
    }
}

impl Picker {
    pub(super) fn new(view: &View) -> Self {
        let active = matches!(
            view.display,
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles
        );
        Self {
            active,
            by: view.groups.by,
            patterns: view
                .groups
                .host_groups
                .iter()
                .filter(|pattern| !pattern.trim().is_empty())
                .map(|pattern| Pattern::new(pattern))
                .collect(),
            var: view.groups.custom_var_name().to_owned(),
            each: view.display != ViewDisplay::HostGroupGrid || view.grid.host_in_each_group,
        }
    }

    /// Whether `host` is in a group the view shows (always, for views
    /// that show no groups).
    pub(super) fn includes(&self, host: &Host) -> bool {
        !self.active || !self.groups_of(host).is_empty()
    }

    /// The groups the view shows `host` in, in the host's own order, each
    /// once (only the first unless a host shows in each of its groups).
    pub(super) fn groups_of(&self, host: &Host) -> Vec<String> {
        if !self.active {
            return Vec::new();
        }
        let mut groups: Vec<String> = Vec::new();
        match self.by {
            GroupSource::HostGroup => {
                for group in &host.groups {
                    let picked = self.patterns.is_empty()
                        || self.patterns.iter().any(|pattern| pattern.matches(group));
                    if picked && !groups.contains(group) {
                        groups.push(group.clone());
                    }
                }
            }
            GroupSource::CustomVar => {
                if let Some(value) = host.vars.get(&self.var) {
                    for value in var_values(value) {
                        if !groups.contains(&value) {
                            groups.push(value);
                        }
                    }
                }
            }
        }
        if !self.each {
            groups.truncate(1);
        }
        groups
    }
}

/// The groups a custom variable's value files a host under: a string, a
/// number or a boolean as text, or each of an array's (non-empty).
fn var_values(value: &serde_json::Value) -> Vec<String> {
    fn scalar(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::String(text) => Some(text.clone()),
            serde_json::Value::Number(number) => Some(number.to_string()),
            serde_json::Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        }
    }
    let values: Vec<String> = match value {
        serde_json::Value::Array(items) => items.iter().filter_map(scalar).collect(),
        other => scalar(other).into_iter().collect(),
    };
    values
        .into_iter()
        .filter(|value| !value.trim().is_empty())
        .collect()
}

/// A group's label: a host group's display name, or the variable's value.
fn labels(data: &Data, by: GroupSource) -> HashMap<&str, &str> {
    match by {
        GroupSource::HostGroup => data
            .host_groups
            .iter()
            .filter(|group| !group.display_name.is_empty())
            .map(|group| (group.name.as_str(), group.display_name.as_str()))
            .collect(),
        GroupSource::CustomVar => HashMap::new(),
    }
}

/// How bad a problem is, for picking a square's state and ordering
/// groups: unhandled problems before handled ones, then the redder state
/// ([`CheckableState::severity_rank`], the order of every dot), then
/// Icinga's severity among equal states.
type Rank = (bool, u8, u32);

/// The [`Rank`] of an object in `state`.
fn rank(unhandled_problem: bool, state: CheckableState, severity: u32) -> Rank {
    (unhandled_problem, state.severity_rank(), severity)
}

/// A grid with its counts.
pub(super) struct BuiltGrid {
    pub(super) grid: Grid,
    /// Every host on it, and its services when they colour it.
    pub(super) summary: Summary,
    /// The squares that don't count as handled (each host once).
    pub(super) counts: Summary,
    /// The squares that count as handled (each host once).
    pub(super) handled: u32,
}

/// Builds a grid from the board's member hosts.
pub(super) fn grid(board: &Board, picker: &Picker, data: &Data) -> BuiltGrid {
    let worst_of_services = board.view.grid.colour == GridColour::WorstOfHostAndServices;
    let mut hosts: Vec<&HostName> = board
        .members
        .keys()
        .filter_map(|object| match object {
            ObjectKey::Host { name } => Some(name),
            ObjectKey::Service { .. } => None,
        })
        .collect();
    hosts.sort();
    let labels = labels(data, board.view.groups.by);
    let mut summary = Tally::default();
    let mut counts = Tally::default();
    let mut handled = 0_u32;
    let mut groups: BTreeMap<String, (GridGroup, Tally, Rank)> = BTreeMap::new();
    let mut shown = 0_u32;
    for name in hosts {
        let Some(host) = data.hosts.get(name) else {
            continue;
        };
        summary.add_host(host);
        let services = data
            .services
            .range(service_range(name))
            .take_while(|(key, _)| &key.host == name)
            .map(|(_, service)| service);
        let (cell, rank) = if worst_of_services {
            let services: Vec<_> = services.collect();
            for service in &services {
                summary.add_service(service, Some(host));
            }
            cell_of(host, &services)
        } else {
            cell_of(host, &[])
        };
        let in_groups = picker.groups_of(host);
        if in_groups.is_empty() {
            continue;
        }
        shown += 1;
        if cell.handled {
            handled += 1;
        } else {
            counts.add(cell.state, false);
        }
        for group in in_groups {
            let label = labels
                .get(group.as_str())
                .map_or_else(|| group.clone(), |label| (*label).to_owned());
            let (entry, tally, worst) = groups.entry(group.clone()).or_insert_with(|| {
                (
                    GridGroup {
                        name: group,
                        label,
                        ..GridGroup::default()
                    },
                    Tally::default(),
                    (false, 0, 0),
                )
            });
            if !cell.handled {
                tally.add(cell.state, false);
            }
            *worst = (*worst).max(rank);
            entry.cells.push(cell.clone());
        }
    }
    let mut groups: Vec<(GridGroup, GroupRank)> = groups
        .into_values()
        .map(|(mut group, tally, worst)| {
            group.counts = tally.finish();
            let rank = group_rank(&group.counts, worst);
            (group, rank)
        })
        .filter(|(group, _)| {
            !board.view.grid.hide_healthy_groups || group.cells.iter().any(|cell| !is_ok(cell))
        })
        .collect();
    sort_groups(&mut groups, board.view.groups.order, |group| &group.label);
    BuiltGrid {
        grid: Grid {
            groups: groups.into_iter().map(|(group, _)| group).collect(),
            hosts: shown,
        },
        summary: summary.finish(),
        counts: counts.finish(),
        handled,
    }
}

/// How a group compares for *worst first*: the colour of its worst
/// unhandled problem as its counts show it (red: critical or down, then
/// purple: unknown or unreachable, then yellow: warning), then how many
/// unhandled problems have that colour, then its worst problem's [`Rank`]
/// (handled ones too, so a group with only handled problems still comes
/// before an all-OK one).
type GroupRank = (u8, u32, Rank);

/// The [`GroupRank`] of a group with these unhandled `counts` and worst
/// problem `worst`.
fn group_rank(counts: &Summary, worst: Rank) -> GroupRank {
    let (colour, count) = [
        (3, counts.critical + counts.down),
        (2, counts.unknown + counts.unreachable),
        (1, counts.warning),
    ]
    .into_iter()
    .find(|&(_, count)| count > 0)
    .unwrap_or((0, 0));
    (colour, count, worst)
}

/// Orders groups worst first ([`GroupRank`], then by label) or by label.
fn sort_groups<G>(groups: &mut [(G, GroupRank)], order: GroupOrder, label: impl Fn(&G) -> &str) {
    groups.sort_by(|(a, a_rank), (b, b_rank)| {
        let by_label = label(a).cmp(label(b));
        match order {
            GroupOrder::WorstFirst => b_rank.cmp(a_rank).then(by_label),
            GroupOrder::Name => by_label,
        }
    });
}

/// Whether a square is green: its state is OK or UP.
fn is_ok(cell: &GridCell) -> bool {
    matches!(
        cell.state,
        CheckableState::Host(HostState::Up) | CheckableState::Service(ServiceState::Ok)
    )
}

/// A host's square. A host that is down or unreachable is its square
/// (its services' problems follow from it, and Icinga counts them as
/// handled). Otherwise the worst unhandled problem of `services`, else the
/// worst handled one, else the host's own state (ties go to the host).
/// Returns the cell and the rank of its state.
fn cell_of(host: &Host, services: &[&Arc<ic_model::Service>]) -> (GridCell, Rank) {
    let host_problem = host.is_problem();
    let host_handled = host.counts_as_handled();
    if host_problem {
        return (
            GridCell {
                host: host.name.clone(),
                state: CheckableState::Host(host.state),
                handled: host_handled,
                problems: u32::from(!host_handled),
                worst_service: None,
            },
            rank(
                !host_handled,
                CheckableState::Host(host.state),
                host.severity(),
            ),
        );
    }
    let mut problems = 0;
    let mut best: (Rank, CheckableState, bool, Option<Arc<str>>) = (
        rank(false, CheckableState::Host(host.state), host.severity()),
        CheckableState::Host(host.state),
        false,
        None,
    );
    for service in services {
        let problem = service.is_problem();
        let handled = service.counts_as_handled(false);
        if problem && !handled {
            problems += 1;
        }
        let service_rank = rank(
            problem && !handled,
            CheckableState::Service(service.state),
            service.severity(),
        );
        if service_rank > best.0 {
            best = (
                service_rank,
                CheckableState::Service(service.state),
                problem && handled,
                (service.state != ServiceState::Ok).then(|| Arc::clone(&service.key.name)),
            );
        }
    }
    let (best_rank, state, handled_problem, worst_service) = best;
    // Nothing wrong: hollow when the host is in downtime (a hollow green
    // square).
    let handled = if state.is_problem() {
        handled_problem
    } else {
        host.check.in_downtime()
    };
    (
        GridCell {
            host: host.name.clone(),
            state,
            handled,
            problems,
            worst_service,
        },
        best_rank,
    )
}

/// Tiles with their counts.
pub(super) struct BuiltTiles {
    pub(super) tiles: Vec<Tile>,
    /// Every object in a tile, each once.
    pub(super) summary: Summary,
    /// The objects that don't count as handled, each once.
    pub(super) counts: Summary,
    /// The objects that count as handled, each once.
    pub(super) handled: u32,
}

/// Builds tiles from the board's members: a tile per group, counting the
/// members in it.
pub(super) fn tiles(board: &Board, picker: &Picker, data: &Data) -> BuiltTiles {
    /// A tile while it is counted.
    struct Building {
        tile: Tile,
        tally: Tally,
        /// The objects that don't count as handled problems.
        unhandled: Tally,
        worst: Rank,
    }
    let labels = labels(data, board.view.groups.by);
    let mut summary = Tally::default();
    let mut counts = Tally::default();
    let mut handled = 0_u32;
    let mut tiles: Vec<Building> = Vec::new();
    let mut index_of: HashMap<String, usize> = HashMap::new();
    // Each host's tiles, worked out once per host (a host has many
    // services).
    let mut host_tiles: HashMap<&HostName, Vec<usize>> = HashMap::new();
    for (object, facts) in &board.members {
        let name = object.host_name();
        if !host_tiles.contains_key(name) {
            let Some(host) = data.hosts.get(name) else {
                continue;
            };
            let indices = picker
                .groups_of(host)
                .into_iter()
                .map(|group| {
                    let index = *index_of.entry(group.clone()).or_insert_with(|| {
                        let label = labels
                            .get(group.as_str())
                            .map_or_else(|| group.clone(), |label| (*label).to_owned());
                        tiles.push(Building {
                            tile: Tile {
                                name: group,
                                label,
                                ..Tile::default()
                            },
                            tally: Tally::default(),
                            unhandled: Tally::default(),
                            worst: (false, 0, 0),
                        });
                        tiles.len() - 1
                    });
                    tiles[index].tile.hosts += 1;
                    index
                })
                .collect();
            host_tiles.insert(name, indices);
        }
        summary.add(facts.state, facts.handled);
        if facts.handled {
            handled += 1;
        } else {
            counts.add(facts.state, false);
        }
        let object_rank = rank(facts.problem && !facts.handled, facts.state, facts.severity);
        for &index in host_tiles.get(name).into_iter().flatten() {
            let building = &mut tiles[index];
            building.tally.add(facts.state, facts.handled);
            if !(facts.problem && facts.handled) {
                building.unhandled.add(facts.state, false);
            }
            building.worst = building.worst.max(object_rank);
        }
    }
    let mut tiles: Vec<(Tile, GroupRank)> = tiles
        .into_iter()
        .map(|mut building| {
            building.tile.summary = building.tally.finish();
            building.tile.counts = building.unhandled.finish();
            let rank = group_rank(&building.tile.counts, building.worst);
            (building.tile, rank)
        })
        .collect();
    sort_groups(&mut tiles, board.view.groups.order, |tile| &tile.label);
    BuiltTiles {
        tiles: tiles.into_iter().map(|(tile, _)| tile).collect(),
        summary: summary.finish(),
        counts: counts.finish(),
        handled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str, text: &str) -> bool {
        Pattern::new(pattern).matches(text)
    }

    #[test]
    fn globs_match_like_icinga() {
        assert!(glob("pg-*", "pg-orders"));
        assert!(glob("pg-*", "pg-"));
        assert!(!glob("pg-*", "mysql-pg-orders"));
        assert!(glob("*-cache", "redis-cache"));
        assert!(glob("db-?", "db-1"));
        assert!(!glob("db-?", "db-10"));
        assert!(glob("*a*b*", "xxaxxbxx"));
        assert!(!glob("*a*b", "xxaxxbxx"));
        assert!(glob("*", ""));
        assert!(
            glob("PG-*", "pg-orders"),
            "ASCII case is ignored, as Icinga"
        );
        assert!(glob(" linux ", "linux"), "a picked name is trimmed");
        assert!(
            !glob("linux", "Linux"),
            "a picked name is that group only (review of 025864c)"
        );
        assert!(
            glob("linux*", "Linux-hosts"),
            "a pattern ignores ASCII case"
        );
        assert!(!glob("linux", "linux-hosts"));
        assert!(glob(r"a\*", "a*"), "\\* is a literal star");
    }

    #[test]
    fn custom_var_values_become_groups() {
        assert_eq!(var_values(&serde_json::json!("ams")), ["ams"]);
        assert_eq!(var_values(&serde_json::json!(3)), ["3"]);
        assert_eq!(
            var_values(&serde_json::json!(["ams", 2, "", null])),
            ["ams", "2"]
        );
        assert!(var_values(&serde_json::json!({ "a": 1 })).is_empty());
        assert!(var_values(&serde_json::json!(" ")).is_empty());
    }
}
