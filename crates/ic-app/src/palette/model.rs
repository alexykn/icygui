//! What the command palette offers and finds (UI-03): commands (actions on
//! the focused or marked objects, reload, new dashboard or group, import
//! and export, pausing notifications, the sidebar), dashboards, hosts,
//! services, environments to switch to, and their settings. Pure, so it's
//! tested without a window.
//!
//! The candidates are indexed (lowercased) once when the palette opens, so
//! a keystroke over 30 000 services costs one pass of the matcher; only
//! the best few per section are kept.

use std::cmp::Reverse;
use std::time::Duration;

use ic_model::{CheckableState, ObjectKey, Timestamp};
use ic_rules::DashboardRef;

use super::fuzzy::{Match, Query};
use crate::actions::ObjectAction;
use crate::app_state::AppState;
use crate::sidebar::Dot;

/// The palette's sections, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Section {
    /// Things to do.
    Commands,
    /// Dashboards to show.
    Dashboards,
    /// Hosts to open.
    Hosts,
    /// Services to open.
    Services,
    /// Environments to switch to, and environment settings.
    Environments,
}

impl Section {
    /// The section's heading.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Commands => "commands",
            Self::Dashboards => "dashboards",
            Self::Hosts => "hosts",
            Self::Services => "services",
            Self::Environments => "environments",
        }
    }

    /// How many results the section shows at most.
    fn limit(self, searching: bool) -> usize {
        match self {
            Self::Services if searching => 8,
            Self::Environments if searching => 4,
            Self::Commands | Self::Dashboards if !searching => 8,
            _ => 6,
        }
    }
}

/// How long notifications can be paused for from the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pause {
    /// For a while.
    For(Duration),
    /// Until 08:00 tomorrow, local time.
    UntilTomorrow,
}

/// What running a palette item does.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaletteCommand {
    /// Show a dashboard.
    ShowDashboard(DashboardRef),
    /// Show an object: in a dashboard listing it, else as a tab.
    OpenObject(ObjectKey),
    /// Open an object as a tab.
    OpenTab(ObjectKey),
    /// Run an action on these objects (an empty list: the focused or
    /// marked ones).
    Act(ObjectAction, Vec<ObjectKey>),
    /// Copy text (`what` names it in the confirmation: `the filter
    /// expression`).
    Copy {
        /// What it is.
        what: &'static str,
        /// The text.
        text: String,
    },
    /// Reload from Icinga.
    Reload,
    /// Show or hide the sidebar.
    ToggleSidebar,
    /// Create a dashboard in the selected dashboard's group.
    NewDashboard,
    /// Create a group.
    NewGroup,
    /// Edit the selected dashboard.
    EditDashboard(DashboardRef),
    /// Import dashboards from a file.
    ImportDashboards,
    /// Export every group to a file.
    ExportDashboards,
    /// Pause notifications.
    Pause(Pause),
    /// Resume paused notifications.
    Resume,
    /// Make this environment the active one.
    SwitchEnvironment(String),
    /// Add an environment.
    AddEnvironment,
    /// Edit this environment.
    EditEnvironment(String),
}

impl PaletteCommand {
    /// The object a command opens, if it opens one (`tab` opens it as a
    /// tab instead).
    pub(crate) fn object(&self) -> Option<&ObjectKey> {
        match self {
            Self::OpenObject(key) | Self::OpenTab(key) => Some(key),
            Self::Act(_, targets) if targets.len() == 1 => targets.first(),
            _ => None,
        }
    }
}

/// One result.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaletteItem {
    /// Its section.
    pub(crate) section: Section,
    /// The main text.
    pub(crate) label: String,
    /// Faint text after it (`on db-prod-03 · critical`).
    pub(crate) detail: String,
    /// A state dot, for objects and dashboards.
    pub(crate) dot: Option<Dot>,
    /// The key that does the same, if any.
    pub(crate) key_hint: Option<&'static str>,
    /// What it does.
    pub(crate) command: PaletteCommand,
    /// The label's characters the query matched, for highlighting.
    pub(crate) matched: Vec<usize>,
    /// Why the API user may not run it (ENV-09): shown faint with the
    /// reason; running it says so in a toast.
    pub(crate) denied: Option<String>,
}

/// One indexed candidate.
#[derive(Clone, Debug)]
struct Candidate {
    item: PaletteItem,
    /// The label and detail, lowercased and split into characters,
    /// matched against the query.
    haystack: Vec<char>,
    /// Where the label ends in the haystack (in characters), so only the
    /// label's matches are highlighted.
    label_chars: usize,
    /// For objects: problems first when the query names a verb.
    severity: u32,
    /// For objects: whether it's in a problem state (only problems can be
    /// acknowledged).
    problem: bool,
}

/// What the palette searches, built when it opens.
#[derive(Clone, Debug, Default)]
pub(crate) struct PaletteIndex {
    commands: Vec<Candidate>,
    dashboards: Vec<Candidate>,
    hosts: Vec<Candidate>,
    services: Vec<Candidate>,
    environments: Vec<Candidate>,
    /// Why the API user may not run the verbs' actions (ENV-09).
    verb_denials: Vec<(ObjectAction, String)>,
}

/// The objects the palette's actions apply to: the marked rows, else the
/// pane's or the cursor's object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Focus {
    /// The objects, in list order.
    pub(crate) targets: Vec<ObjectKey>,
}

/// The verbs a query may start with to act on the objects it names
/// (`ack db-prod`).
const VERBS: [(&str, ObjectAction); 8] = [
    ("ack", ObjectAction::Acknowledge),
    ("acknowledge", ObjectAction::Acknowledge),
    ("downtime", ObjectAction::ScheduleDowntime),
    ("dt", ObjectAction::ScheduleDowntime),
    ("check", ObjectAction::CheckNow),
    ("recheck", ObjectAction::CheckNow),
    ("comment", ObjectAction::AddComment),
    ("note", ObjectAction::AddComment),
];

impl PaletteIndex {
    /// Indexes what `state` offers now; actions apply to `focus`.
    pub(crate) fn build(state: &AppState, focus: &Focus, now: Timestamp) -> Self {
        Self {
            commands: commands(state, focus, now)
                .into_iter()
                .map(|item| candidate(item, 0))
                .collect(),
            dashboards: dashboard_candidates(state),
            hosts: host_candidates(state),
            services: service_candidates(state),
            environments: environment_candidates(state),
            verb_denials: VERBS
                .iter()
                .filter_map(|(_, action)| {
                    state
                        .action_denial(action)
                        .map(|denial| (action.clone(), denial))
                })
                .collect(),
        }
    }

    /// The results for `query`, by section, best first within each.
    pub(crate) fn search(&self, query: &str) -> Vec<PaletteItem> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            let mut items = Vec::new();
            for (section, candidates) in [
                (Section::Commands, &self.commands),
                (Section::Dashboards, &self.dashboards),
                (Section::Environments, &self.environments),
            ] {
                items.extend(
                    candidates
                        .iter()
                        .take(section.limit(false))
                        .map(|candidate| candidate.item.clone()),
                );
            }
            return items;
        }
        // Commands match the whole query (`check now`); after a verb, the
        // actions on the objects it names follow, and the objects are
        // searched for the rest (`ack db-prod` lists db-prod's objects,
        // not every `backup`).
        let mut items = best(
            &self.commands,
            &Query::new(&query),
            Section::Commands.limit(true),
            Rank::Score,
        );
        let objects_query = match verb(&query) {
            Some((action, rest)) if !rest.is_empty() => {
                let rest = Query::new(rest);
                items.extend(self.act_on(&action, &rest));
                rest
            }
            _ => Query::new(&query),
        };
        for (section, candidates) in [
            (Section::Dashboards, &self.dashboards),
            (Section::Hosts, &self.hosts),
            (Section::Services, &self.services),
            (Section::Environments, &self.environments),
        ] {
            items.extend(best(
                candidates,
                &objects_query,
                section.limit(true),
                Rank::Score,
            ));
        }
        items
    }

    /// "Acknowledge · <object>" for the objects `rest` names, problems
    /// first (only problems for acknowledgements: Icinga refuses the
    /// others).
    fn act_on(&self, action: &ObjectAction, rest: &Query) -> Vec<PaletteItem> {
        let label = action_label(action);
        let rank = if *action == ObjectAction::Acknowledge {
            Rank::ProblemsOnly
        } else {
            Rank::ProblemsFirst
        };
        let denied = self
            .verb_denials
            .iter()
            .find(|(denied, _)| denied == action)
            .map(|(_, denial)| denial.clone());
        let pick = |candidates: &[Candidate], limit| best(candidates, rest, limit, rank);
        let services = pick(&self.services, 4);
        let hosts = pick(&self.hosts, 2);
        services
            .into_iter()
            .chain(hosts)
            .filter_map(|object| {
                let key = object.command.object()?.clone();
                Some(PaletteItem {
                    section: Section::Commands,
                    label: format!("{label} · {}", object.label),
                    detail: object.detail,
                    dot: object.dot,
                    key_hint: None,
                    command: PaletteCommand::Act(action.clone(), vec![key]),
                    matched: Vec::new(),
                    denied: denied.clone(),
                })
            })
            .collect()
    }
}

/// How [`best`] orders and filters matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rank {
    /// By match score.
    Score,
    /// More severe objects first, then by score.
    ProblemsFirst,
    /// Only objects in a problem state, more severe first.
    ProblemsOnly,
}

/// The best `limit` candidates for `query`, ranked by `rank`.
fn best(candidates: &[Candidate], query: &Query, limit: usize, rank: Rank) -> Vec<PaletteItem> {
    if limit == 0 {
        return Vec::new();
    }
    let mut found: Vec<(i64, usize, Match)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| rank != Rank::ProblemsOnly || candidate.problem)
        .filter_map(|(index, candidate)| {
            let found = query.matches(&candidate.haystack)?;
            let rank = match rank {
                Rank::Score => i64::from(found.score),
                Rank::ProblemsFirst | Rank::ProblemsOnly => {
                    i64::from(candidate.severity) * 1_000 + i64::from(found.score)
                }
            };
            Some((rank, index, found))
        })
        .collect();
    // Best first; ties keep the index order (stable).
    found.sort_by_key(|(rank, index, _)| (Reverse(*rank), *index));
    found
        .into_iter()
        .take(limit)
        .map(|(_, index, found)| {
            let candidate = &candidates[index];
            let mut item = candidate.item.clone();
            item.matched = found
                .positions
                .into_iter()
                .filter(|position| *position < candidate.label_chars)
                .collect();
            item
        })
        .collect()
}

/// The verb a query starts with and the rest (`ack db-prod` →
/// acknowledge, `db-prod`).
pub(crate) fn verb(query: &str) -> Option<(ObjectAction, &str)> {
    let (first, rest) = query.split_once(char::is_whitespace)?;
    VERBS
        .iter()
        .find(|(word, _)| *word == first)
        .map(|(_, action)| (action.clone(), rest.trim()))
}

fn candidate(item: PaletteItem, severity: u32) -> Candidate {
    object_candidate(item, severity, false)
}

fn object_candidate(item: PaletteItem, severity: u32, problem: bool) -> Candidate {
    let label = item.label.to_lowercase();
    let label_chars = label.chars().count();
    let mut haystack: Vec<char> = label.chars().collect();
    if !item.detail.is_empty() {
        haystack.push(' ');
        haystack.extend(item.detail.to_lowercase().chars());
    }
    Candidate {
        item,
        haystack,
        label_chars,
        severity,
        problem,
    }
}

fn setting(label: String, detail: &str, command: PaletteCommand) -> PaletteItem {
    PaletteItem {
        section: Section::Environments,
        label,
        detail: detail.to_owned(),
        dot: None,
        key_hint: None,
        command,
        matched: Vec::new(),
        denied: None,
    }
}

fn join_detail(first: &str, second: &str) -> String {
    if first.is_empty() {
        second.to_owned()
    } else {
        format!("{first} · {second}")
    }
}

/// An action's label.
fn action_label(action: &ObjectAction) -> &'static str {
    match action {
        ObjectAction::Acknowledge => "Acknowledge",
        ObjectAction::ScheduleDowntime => "Schedule downtime",
        ObjectAction::CheckNow => "Check now",
        ObjectAction::AddComment => "Add comment",
        ObjectAction::RemoveAcknowledgement => "Remove acknowledgement",
        ObjectAction::RemoveComment(_) => "Remove comment",
        ObjectAction::RemoveDowntime(_) => "Remove downtime",
        ObjectAction::RemoveDowntimes => "Remove downtimes",
        ObjectAction::SubmitCheckResult => "Submit check result…",
        ObjectAction::RunCommand => "Run command…",
    }
}

/// Every dashboard of the environment, with its group and unhandled count.
fn dashboard_candidates(state: &AppState) -> Vec<Candidate> {
    let Some(environment) = state.environment() else {
        return Vec::new();
    };
    let mut candidates = Vec::new();
    for group in &environment.groups {
        for dashboard in &group.dashboards {
            let reference = DashboardRef {
                group_id: group.id.clone(),
                dashboard_id: dashboard.id.clone(),
            };
            let summary = state.result(&reference).map(|result| &result.summary);
            let unhandled = summary.map_or(0, |summary| summary.unhandled);
            let detail = if unhandled > 0 {
                format!("{} · {unhandled} unhandled", group.name)
            } else {
                group.name.clone()
            };
            candidates.push(candidate(
                PaletteItem {
                    section: Section::Dashboards,
                    label: dashboard.name.clone(),
                    detail,
                    dot: Some(Dot::from_summary(summary)),
                    key_hint: None,
                    command: PaletteCommand::ShowDashboard(reference),
                    matched: Vec::new(),
                    denied: None,
                },
                0,
            ));
        }
    }
    candidates
}

/// Every host: display name, with name, address and state after it.
fn host_candidates(state: &AppState) -> Vec<Candidate> {
    state
        .snapshot()
        .hosts
        .values()
        .map(|host| {
            let state = CheckableState::Host(host.state);
            let address = if host.display_name == host.name.as_str() {
                host.address.clone()
            } else {
                format!("{} {}", host.name, host.address).trim().to_owned()
            };
            let detail = join_detail(&address, crate::format::state_word(state));
            object_candidate(
                PaletteItem {
                    section: Section::Hosts,
                    label: host.display_name.clone(),
                    detail,
                    dot: Some(Dot::for_object(Some(state))),
                    key_hint: None,
                    command: PaletteCommand::OpenObject(host.key()),
                    matched: Vec::new(),
                    denied: None,
                },
                host.severity(),
                host.is_problem(),
            )
        })
        .collect()
}

/// Every service: display name, with its host and state after it.
fn service_candidates(state: &AppState) -> Vec<Candidate> {
    let snapshot = state.snapshot();
    snapshot
        .services
        .values()
        .map(|service| {
            let state = CheckableState::Service(service.state);
            let host = snapshot.host_of(&service.key).map_or_else(
                || service.key.host.to_string(),
                |host| host.display_name.clone(),
            );
            object_candidate(
                PaletteItem {
                    section: Section::Services,
                    label: service.display_name.clone(),
                    detail: join_detail(&format!("on {host}"), crate::format::state_word(state)),
                    dot: Some(Dot::for_object(Some(state))),
                    key_hint: None,
                    command: PaletteCommand::OpenObject(service.object_key()),
                    matched: Vec::new(),
                    denied: None,
                },
                service.severity(),
                service.is_problem(),
            )
        })
        .collect()
}

/// Switching to each other environment, editing this one, adding one.
fn environment_candidates(state: &AppState) -> Vec<Candidate> {
    let active = state.active_environment_id();
    let mut candidates: Vec<Candidate> = state
        .environments()
        .iter()
        .filter(|environment| active != Some(environment.id.as_str()))
        .map(|environment| {
            candidate(
                PaletteItem {
                    section: Section::Environments,
                    label: format!("Switch to {}", environment.name),
                    detail: environment.url.clone(),
                    dot: None,
                    key_hint: None,
                    command: PaletteCommand::SwitchEnvironment(environment.id.clone()),
                    matched: Vec::new(),
                    denied: None,
                },
                0,
            )
        })
        .collect();
    if let Some(environment) = state.environment() {
        candidates.push(candidate(
            setting(
                format!("Edit environment {}…", environment.name),
                "settings: URL, login, TLS",
                PaletteCommand::EditEnvironment(environment.id.clone()),
            ),
            0,
        ));
    }
    candidates.push(candidate(
        setting(
            "Add environment…".to_owned(),
            "settings",
            PaletteCommand::AddEnvironment,
        ),
        0,
    ));
    candidates
}

/// A command item.
fn command(
    label: &str,
    detail: String,
    key_hint: Option<&'static str>,
    command: PaletteCommand,
) -> PaletteItem {
    PaletteItem {
        section: Section::Commands,
        label: label.to_owned(),
        detail,
        dot: None,
        key_hint,
        command,
        matched: Vec::new(),
        denied: None,
    }
}

/// The commands: actions on the focus, then the rest.
fn commands(state: &AppState, focus: &Focus, now: Timestamp) -> Vec<PaletteItem> {
    let (mut items, more_actions) = action_commands(state, focus);
    let has_environment = state.environment().is_some();
    if has_environment {
        items.extend(dashboard_commands(state));
    }
    items.extend(more_actions);
    items.extend(pause_commands(state, now, has_environment));
    items.push(command(
        "Toggle sidebar",
        String::new(),
        Some(if cfg!(target_os = "macos") {
            "⌘B"
        } else {
            "ctrl-b"
        }),
        PaletteCommand::ToggleSidebar,
    ));
    if has_environment {
        items.push(command(
            "Import dashboards…",
            "from a file".to_owned(),
            None,
            PaletteCommand::ImportDashboards,
        ));
        items.push(command(
            "Export dashboards…",
            "every group, to a file".to_owned(),
            None,
            PaletteCommand::ExportDashboards,
        ));
    }
    items
}

/// The actions on the focused objects, with their keys, then copying
/// their names and filter expression (PANE-05). Actions the API user may
/// not run say why (ENV-09).
fn action_commands(state: &AppState, focus: &Focus) -> (Vec<PaletteItem>, Vec<PaletteItem>) {
    let what = match focus.targets.as_slice() {
        [] => return (Vec::new(), Vec::new()),
        [one] => one.to_string(),
        many => format!("{} objects", many.len()),
    };
    let snapshot = state.snapshot();
    let mut actions = vec![
        (ObjectAction::Acknowledge, Some("a")),
        (ObjectAction::ScheduleDowntime, Some("d")),
        (ObjectAction::CheckNow, Some("r")),
        (ObjectAction::AddComment, Some("c")),
        (ObjectAction::SubmitCheckResult, None),
        (ObjectAction::RunCommand, None),
    ];
    // Removals only where there is something to remove.
    let acknowledged = focus.targets.iter().any(|target| match target {
        ObjectKey::Host { name } => snapshot
            .hosts
            .get(name)
            .is_some_and(|host| host.check.acknowledgement.is_acknowledged()),
        ObjectKey::Service { key } => snapshot
            .services
            .get(key)
            .is_some_and(|service| service.check.acknowledgement.is_acknowledged()),
    });
    if acknowledged {
        actions.push((ObjectAction::RemoveAcknowledgement, None));
    }
    if focus.targets.iter().any(|target| {
        snapshot
            .downtimes
            .get(target)
            .is_some_and(|list| !list.is_empty())
    }) {
        actions.push((ObjectAction::RemoveDowntimes, None));
    }
    let mut items: Vec<PaletteItem> = actions
        .into_iter()
        .map(|(action, key)| {
            let denied = state.action_denial(&action);
            let mut item = command(
                action_label(&action),
                what.clone(),
                key,
                PaletteCommand::Act(action, Vec::new()),
            );
            item.denied = denied;
            item
        })
        .collect();
    let names = if focus.targets.len() == 1 {
        "Copy name"
    } else {
        "Copy names"
    };
    items.push(command(
        names,
        what.clone(),
        None,
        PaletteCommand::Copy {
            what: if focus.targets.len() == 1 {
                "the name"
            } else {
                "the names"
            },
            text: crate::operate::expression::names(&focus.targets),
        },
    ));
    items.push(command(
        "Copy filter expression",
        what,
        None,
        PaletteCommand::Copy {
            what: "the filter expression",
            text: crate::operate::expression::filter(&focus.targets),
        },
    ));
    // The keyed actions first; the rest after the other commands, so the
    // palette's first screen keeps reloading and new dashboards.
    let rest = items.split_off(4);
    (items, rest)
}

/// Reloading, and making or editing dashboards and groups.
fn dashboard_commands(state: &AppState) -> Vec<PaletteItem> {
    let mut items = vec![
        command(
            "Reload from Icinga",
            "a lean reload of every object".to_owned(),
            None,
            PaletteCommand::Reload,
        ),
        command(
            "New dashboard",
            String::new(),
            Some(crate::sidebar::new_key()),
            PaletteCommand::NewDashboard,
        ),
    ];
    if let Some(selected) = state.selected()
        && let Some((_, dashboard)) = state.dashboard(selected)
    {
        items.push(command(
            "Edit dashboard",
            dashboard.name.clone(),
            None,
            PaletteCommand::EditDashboard(selected.clone()),
        ));
    }
    items.push(command(
        "New group",
        String::new(),
        None,
        PaletteCommand::NewGroup,
    ));
    items
}

/// Resuming paused notifications, or pausing them.
fn pause_commands(state: &AppState, now: Timestamp, has_environment: bool) -> Vec<PaletteItem> {
    match state.paused_until().filter(|until| *until > now) {
        Some(until) => vec![command(
            "Resume notifications",
            format!("paused until {}", crate::format::clock(until, now)),
            None,
            PaletteCommand::Resume,
        )],
        None if has_environment => [
            (
                "Pause notifications for 30 minutes",
                Pause::For(Duration::from_mins(30)),
            ),
            (
                "Pause notifications for 1 hour",
                Pause::For(Duration::from_hours(1)),
            ),
            ("Pause notifications until tomorrow", Pause::UntilTomorrow),
        ]
        .into_iter()
        .map(|(label, pause)| command(label, String::new(), None, PaletteCommand::Pause(pause)))
        .collect(),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000.)
    }

    fn index(focus: &Focus) -> PaletteIndex {
        PaletteIndex::build(&AppState::fixture(now()), focus, now())
    }

    fn labels(items: &[PaletteItem]) -> Vec<&str> {
        items.iter().map(|item| item.label.as_str()).collect()
    }

    /// At production scale (30 000 services) indexing and every keystroke
    /// stay fast enough for the UI thread; the bound is loose for debug
    /// builds on slow CI machines.
    #[test]
    fn thirty_thousand_services_search_quickly() {
        let state = AppState::fixture_with(
            now(),
            crate::fixture::FixtureOptions {
                generated_rows: 30_000,
            },
        );
        let started = std::time::Instant::now();
        let palette = PaletteIndex::build(&state, &Focus::default(), now());
        let built = started.elapsed();
        let mut slowest = Duration::ZERO;
        for query in [
            "d",
            "db",
            "db-prod",
            "replication",
            "ack load",
            "zzzz",
            "load 0123",
        ] {
            let started = std::time::Instant::now();
            let items = palette.search(query);
            slowest = slowest.max(started.elapsed());
            assert!(query == "zzzz" || !items.is_empty(), "{query}");
        }
        println!("index {built:?}, slowest search {slowest:?}");
        assert!(built < Duration::from_secs(3), "{built:?}");
        assert!(slowest < Duration::from_secs(1), "{slowest:?}");
    }

    #[test]
    fn an_empty_query_offers_commands_dashboards_and_environments() {
        let items = index(&Focus::default()).search("");
        assert_eq!(items[0].section, Section::Commands);
        assert!(items.iter().any(|item| item.section == Section::Dashboards));
        assert!(items.iter().any(|item| item.label == "Add environment…"));
        assert!(
            items.iter().all(|item| item.section != Section::Services),
            "objects need a query"
        );
    }

    #[test]
    fn queries_find_services_hosts_and_dashboards() {
        let palette = index(&Focus::default());
        let items = palette.search("replication");
        let service = items
            .iter()
            .find(|item| item.section == Section::Services)
            .unwrap();
        assert_eq!(service.label, "postgres-replication");
        assert!(
            service.detail.starts_with("on db-prod-03"),
            "{}",
            service.detail
        );
        assert_eq!(service.matched, (9..20).collect::<Vec<_>>());
        assert_eq!(
            service.command,
            PaletteCommand::OpenObject(ObjectKey::service("db-prod-03", "postgres-replication"))
        );
        let hosts = palette.search("db-prod-03");
        assert!(
            hosts
                .iter()
                .any(|item| item.section == Section::Hosts && item.label == "db-prod-03"),
            "{:?}",
            labels(&hosts)
        );
        let dashboards = palette.search("netw");
        assert_eq!(dashboards[0].section, Section::Dashboards);
        assert_eq!(dashboards[0].label, "network");
    }

    #[test]
    fn sections_keep_their_order_and_limits() {
        let items = index(&Focus::default()).search("o");
        let sections: Vec<Section> = items.iter().map(|item| item.section).collect();
        let mut sorted = sections.clone();
        sorted.sort();
        assert_eq!(sections, sorted, "grouped by section, in order");
        let services = sections.iter().filter(|s| **s == Section::Services).count();
        assert!(services <= 8);
    }

    #[test]
    fn verbs_act_on_the_objects_named_problems_first() {
        let items = index(&Focus::default()).search("ack db-prod");
        let first = items
            .iter()
            .find(|item| item.label.starts_with("Acknowledge · "))
            .unwrap();
        assert_eq!(first.section, Section::Commands);
        assert!(first.label.starts_with("Acknowledge · "), "{}", first.label);
        let PaletteCommand::Act(ObjectAction::Acknowledge, targets) = &first.command else {
            panic!("{:?}", first.command);
        };
        assert_eq!(
            targets,
            &[ObjectKey::service("db-prod-03", "postgres-replication")],
            "the critical one first"
        );
        assert!(
            items
                .iter()
                .filter(|item| item.label.starts_with("Acknowledge · "))
                .all(|item| { item.dot != Some(Dot::Ok) }),
            "only problems can be acknowledged"
        );
        assert!(
            items
                .iter()
                .filter(|item| item.section == Section::Services)
                .all(|item| item.detail.contains("db-prod")),
            "the objects are searched for the rest of the query"
        );
        assert_eq!(
            verb("dt web"),
            Some((ObjectAction::ScheduleDowntime, "web"))
        );
        assert_eq!(verb("ack"), None);
        assert_eq!(verb("acme thing"), None);
    }

    #[test]
    fn actions_on_the_focus_come_first() {
        let focus = Focus {
            targets: vec![ObjectKey::service("db-prod-03", "postgres-replication")],
        };
        let items = index(&focus).search("");
        assert_eq!(items[0].label, "Acknowledge");
        assert_eq!(items[0].detail, "db-prod-03!postgres-replication");
        assert_eq!(items[0].key_hint, Some("a"));
        assert_eq!(
            items[0].command,
            PaletteCommand::Act(ObjectAction::Acknowledge, Vec::new())
        );
        let check = index(&focus).search("check now");
        assert_eq!(check[0].label, "Check now");
    }

    #[test]
    fn pausing_is_offered_until_paused_then_resuming() {
        let mut state = AppState::fixture(now());
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("pause");
        assert!(
            items
                .iter()
                .any(|item| item.command == PaletteCommand::Pause(Pause::UntilTomorrow))
        );
        state.apply(ic_core::CoreEvent::NotificationsPaused(Some(
            Timestamp::from_unix_seconds(1_790_003_600.),
        )));
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("notif");
        assert!(
            items
                .iter()
                .any(|item| item.command == PaletteCommand::Resume)
        );
        assert!(
            items
                .iter()
                .all(|item| !matches!(item.command, PaletteCommand::Pause(_)))
        );
    }

    #[test]
    fn other_environments_can_be_switched_to() {
        let mut state = AppState::fixture(now());
        let staging = ic_config::Environment::new(
            "staging",
            "https://staging:5665",
            ic_config::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        let id = staging.id.clone();
        state.save_environment(staging, false);
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("staging");
        assert_eq!(items[0].command, PaletteCommand::SwitchEnvironment(id));
        assert_eq!(items[0].label, "Switch to staging");
    }
}
