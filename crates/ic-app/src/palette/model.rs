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

use ic_model::{CheckableState, ObjectKey, Timestamp};
use ic_rules::DashboardRef;

use super::fuzzy::{Match, Query};
use crate::actions::ObjectAction;
use crate::app_state::AppState;
use crate::app_state::environments::url_summary;
use crate::notifications::{MuteChoice, OverrideChange, PauseChoice};
use crate::settings::SettingsTab;
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
    Pause(PauseChoice),
    /// Resume paused notifications.
    Resume,
    /// Mute one environment's notifications for a while (A5), or unmute
    /// them (`None`).
    MuteEnvironment(String, Option<PauseChoice>),
    /// Watch, mute or unmute these objects (NOTE-02).
    Override(OverrideChange, Vec<ObjectKey>),
    /// Open the notification centre (NOTE-05).
    OpenNotifications,
    /// Mark every notification read.
    MarkNotificationsRead,
    /// Open the settings (`secondary-,`), on this tab.
    Settings(SettingsTab),
    /// Show what icygui is.
    About,
    /// Quit the app, even when it keeps running in the tray.
    Quit,
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
    /// It acts on several objects (a verb's *all N matches*): the dot's
    /// slot shows a several-objects mark instead, in `dot`'s colour (the
    /// worst state among them).
    pub(crate) several: bool,
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
    /// Ranks below equal matches by [`SECONDARY_PENALTY`]: muting an
    /// environment never outranks switching to it (`staging` selects
    /// "Switch to staging").
    secondary: bool,
}

/// How far a secondary command ranks below an equal match: more than a
/// label's length or start can make up.
const SECONDARY_PENALTY: i64 = 16;

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
                .map(|item| {
                    let secondary = matches!(item.command, PaletteCommand::MuteEnvironment(..));
                    Candidate {
                        secondary,
                        ..candidate(item, 0)
                    }
                })
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
        let mut commands = best(
            &self.commands,
            &Query::new(&query),
            Section::Commands.limit(true),
            Rank::Score,
        );
        let objects_query = match verb(&query) {
            Some((action, rest)) if !rest.is_empty() => {
                let rest = Query::new(rest);
                let acts = self.act_on(&action, &rest);
                if !acts.is_empty() {
                    // What the verb asked for comes first.
                    commands.top = Some(i64::MAX);
                    commands.items.extend(acts);
                }
                rest
            }
            _ => Query::new(&query),
        };
        let mut sections = vec![commands];
        for (section, candidates) in [
            (Section::Dashboards, &self.dashboards),
            (Section::Hosts, &self.hosts),
            (Section::Services, &self.services),
            (Section::Environments, &self.environments),
        ] {
            sections.push(best(
                candidates,
                &objects_query,
                section.limit(true),
                Rank::Score,
            ));
        }
        // The section with the best match first, so Enter runs it: `prod`
        // selects "Switch to prod-cluster", not a scattered match in
        // "Import dashboards…". Ties keep the usual order.
        sections.sort_by_key(|found| Reverse(found.top));
        sections.into_iter().flat_map(|found| found.items).collect()
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
        let pick = |candidates: &[Candidate], limit| best(candidates, rest, limit, rank).items;
        let services = pick(&self.services, 4);
        let hosts = pick(&self.hosts, 2);
        let mut items: Vec<PaletteItem> = services
            .into_iter()
            .chain(hosts)
            .filter_map(|object| {
                let key = object.command.object()?.clone();
                Some(PaletteItem {
                    section: Section::Commands,
                    label: format!("{label} · {}", object.label),
                    detail: object.detail,
                    dot: object.dot,
                    several: false,
                    key_hint: None,
                    command: PaletteCommand::Act(action.clone(), vec![key]),
                    matched: Vec::new(),
                    denied: denied.clone(),
                })
            })
            .collect();
        // Every object the rest names as typed, in one request (design
        // 1d's "run on all matches"); the dialog or the confirmation shows
        // how many.
        let services = all_matches(&self.services, rest, rank);
        let hosts = all_matches(&self.hosts, rest, rank);
        let count = services.len() + hosts.len();
        if count > 1 {
            // The mark takes the worst state among them.
            let worst = services
                .iter()
                .chain(&hosts)
                .max_by_key(|candidate| candidate.severity)
                .and_then(|candidate| candidate.item.dot);
            let objects = |candidates: Vec<&Candidate>| -> Vec<ObjectKey> {
                candidates
                    .into_iter()
                    .filter_map(|candidate| candidate.item.command.object().cloned())
                    .collect()
            };
            let detail = [(services.len(), "service"), (hosts.len(), "host")]
                .into_iter()
                .filter(|(count, _)| *count > 0)
                .map(|(count, noun)| format!("{count} {noun}{}", if count == 1 { "" } else { "s" }))
                .collect::<Vec<_>>()
                .join(" · ");
            items.push(PaletteItem {
                section: Section::Commands,
                label: format!("{label} · all {count} matches"),
                detail,
                dot: worst,
                several: true,
                key_hint: Some(ALL_MATCHES_KEY),
                command: PaletteCommand::Act(
                    action.clone(),
                    objects(services)
                        .into_iter()
                        .chain(objects(hosts))
                        .collect(),
                ),
                matched: Vec::new(),
                denied,
            });
        }
        items
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

/// The key that runs a verb on all the objects it names.
pub(crate) const ALL_MATCHES_KEY: &str = if cfg!(target_os = "macos") {
    "⌘↵"
} else {
    "ctrl-↵"
};

/// The "all N matches" item of a verb query among `items`: the only item
/// that acts on several named objects.
pub(crate) fn all_matches_item(items: &[PaletteItem]) -> Option<usize> {
    items.iter().position(
        |item| matches!(&item.command, PaletteCommand::Act(_, targets) if targets.len() > 1),
    )
}

/// Every candidate that contains each of `query`'s terms as typed (not a
/// fuzzy match; problems only for [`Rank::ProblemsOnly`]).
fn all_matches<'a>(candidates: &'a [Candidate], query: &Query, rank: Rank) -> Vec<&'a Candidate> {
    candidates
        .iter()
        .filter(|candidate| rank != Rank::ProblemsOnly || candidate.problem)
        .filter(|candidate| query.contained_in(&candidate.haystack))
        .filter(|candidate| candidate.item.command.object().is_some())
        .collect()
}

/// What [`best`] found: the items, best first, and the best one's rank.
struct Found {
    /// The best item's rank (`None` without items).
    top: Option<i64>,
    items: Vec<PaletteItem>,
}

/// The best `limit` candidates for `query`, ranked by `rank`.
fn best(candidates: &[Candidate], query: &Query, limit: usize, rank: Rank) -> Found {
    if limit == 0 {
        return Found {
            top: None,
            items: Vec::new(),
        };
    }
    let mut found: Vec<(i64, usize, Match)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, candidate)| rank != Rank::ProblemsOnly || candidate.problem)
        .filter_map(|(index, candidate)| {
            let found = query.matches(&candidate.haystack)?;
            let rank = match rank {
                Rank::Score => {
                    i64::from(found.score)
                        - if candidate.secondary {
                            SECONDARY_PENALTY
                        } else {
                            0
                        }
                }
                // A verb's objects: those named as typed before fuzzy
                // near-misses (`ack db-prod` lists db-prod's problems
                // before web-prod's), then the more severe.
                Rank::ProblemsFirst | Rank::ProblemsOnly => {
                    i64::from(query.contained_in(&candidate.haystack)) * 1_000_000_000
                        + i64::from(candidate.severity) * 1_000
                        + i64::from(found.score)
                }
            };
            Some((rank, index, found))
        })
        .collect();
    // Best first; ties keep the index order (stable).
    found.sort_by_key(|(rank, index, _)| (Reverse(*rank), *index));
    let top = found.first().map(|(rank, _, _)| *rank);
    let items = found
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
        .collect();
    Found { top, items }
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
        secondary: false,
    }
}

fn setting(label: String, detail: &str, command: PaletteCommand) -> PaletteItem {
    PaletteItem {
        section: Section::Environments,
        label,
        detail: detail.to_owned(),
        dot: None,
        several: false,
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
                    several: false,
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
                    several: false,
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
                    several: false,
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
                    detail: url_summary(environment),
                    dot: None,
                    several: false,
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
        several: false,
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
    items.extend(override_commands(state, focus, now));
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

/// Resuming paused notifications, or pausing them; the notification
/// centre, its read marks and the settings.
fn pause_commands(state: &AppState, now: Timestamp, has_environment: bool) -> Vec<PaletteItem> {
    let environments = state.environments();
    let several = environments.len() > 1;
    let mut items = match state.paused_until().filter(|until| *until > now) {
        Some(until) => vec![command(
            "Resume notifications",
            crate::notifications::paused_text(environments.len(), until, now),
            None,
            PaletteCommand::Resume,
        )],
        None if has_environment => PauseChoice::ALL
            .into_iter()
            .map(|choice| {
                command(
                    &format!("Pause notifications {}", choice.label(now)),
                    if several {
                        "every environment".to_owned()
                    } else {
                        String::new()
                    },
                    None,
                    PaletteCommand::Pause(choice),
                )
            })
            .collect(),
        None => Vec::new(),
    };
    // With several environments each one mutes on its own too (A5).
    if several {
        for environment in environments {
            let id = &environment.id;
            match state.environment_paused_until(id, now) {
                Some(until) => items.push(command(
                    &format!("Unmute {}", environment.name),
                    format!("muted until {}", crate::notifications::when(until, now)),
                    None,
                    PaletteCommand::MuteEnvironment(id.clone(), None),
                )),
                None => items.extend(PauseChoice::ALL.into_iter().map(|choice| {
                    command(
                        &format!("Mute {} {}", environment.name, choice.label(now)),
                        "the other environments still notify".to_owned(),
                        None,
                        PaletteCommand::MuteEnvironment(id.clone(), Some(choice)),
                    )
                })),
            }
        }
    }
    let unread = state.unread_notifications();
    items.push(command(
        "Notifications",
        if unread > 0 {
            format!("{unread} unread")
        } else {
            String::new()
        },
        None,
        PaletteCommand::OpenNotifications,
    ));
    if unread > 0 {
        items.push(command(
            "Mark all notifications read",
            String::new(),
            None,
            PaletteCommand::MarkNotificationsRead,
        ));
    }
    if has_environment {
        items.push(command(
            "Notification settings…",
            String::new(),
            None,
            PaletteCommand::Settings(SettingsTab::Notifications),
        ));
    }
    items.push(command(
        "Settings…",
        String::new(),
        Some(crate::settings::settings_key()),
        PaletteCommand::Settings(SettingsTab::General),
    ));
    items.push(command(
        "About icygui",
        String::new(),
        None,
        PaletteCommand::About,
    ));
    items.push(command(
        "Quit icygui",
        String::new(),
        Some(crate::settings::quit_key()),
        PaletteCommand::Quit,
    ));
    items
}

/// Watching, muting and unmuting the focused objects (NOTE-02).
fn override_commands(state: &AppState, focus: &Focus, now: Timestamp) -> Vec<PaletteItem> {
    if focus.targets.is_empty() || state.environment().is_none() {
        return Vec::new();
    }
    let what = match focus.targets.as_slice() {
        [one] => one.to_string(),
        many => format!("{} objects", many.len()),
    };
    let current: Vec<_> = focus
        .targets
        .iter()
        .map(|target| state.object_override(target, now).map(|entry| entry.mode))
        .collect();
    let all_watched = current
        .iter()
        .all(|mode| *mode == Some(ic_rules::ObjectMode::Watch));
    let any = current.iter().any(Option::is_some);
    let targets = focus.targets.clone();
    let mut items = Vec::new();
    if !all_watched {
        items.push(command(
            "Watch",
            what.clone(),
            None,
            PaletteCommand::Override(OverrideChange::Watch, targets.clone()),
        ));
    }
    for choice in MuteChoice::ALL {
        items.push(command(
            &format!("Mute {}", choice.label(now)),
            what.clone(),
            None,
            PaletteCommand::Override(OverrideChange::Mute(choice), targets.clone()),
        ));
    }
    if any {
        items.push(command(
            if all_watched {
                "Stop watching"
            } else {
                "Unmute"
            },
            what,
            None,
            PaletteCommand::Override(OverrideChange::Clear, targets),
        ));
    }
    items
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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
    fn sections_stay_together_within_their_limits() {
        let items = index(&Focus::default()).search("o");
        let mut sections: Vec<Section> = items.iter().map(|item| item.section).collect();
        let services = sections.iter().filter(|s| **s == Section::Services).count();
        assert!(services <= 8);
        let listed = sections.len();
        sections.dedup();
        let mut unique = sections.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(sections.len(), unique.len(), "each section in one run");
        assert!(listed > sections.len());
    }

    #[test]
    fn the_section_with_the_best_match_comes_first() {
        let mut state = AppState::fixture(now());
        let other = ic_config::Environment::new(
            "prod-cluster-2",
            "https://prod-2:5665",
            ic_config::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        state.save_environment(other, false);
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("prod");
        // Enter runs the first item: a word-start match, never a
        // scattered one like "Import dashboards…" (i-m-P-o-R-t … O … D).
        assert_ne!(items[0].label, "Import dashboards…");
        assert!(
            items[0].label.to_lowercase().contains("prod"),
            "{:?}",
            items.iter().map(|item| &item.label).collect::<Vec<_>>()
        );
        assert!(
            items
                .iter()
                .any(|item| item.label == "Switch to prod-cluster-2"),
            "{items:?}"
        );
        // Commands still lead when they match best.
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("import");
        assert_eq!(items[0].label, "Import dashboards…");
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
    fn a_verb_runs_on_all_matches_as_typed() {
        let state = AppState::fixture(now());
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("ack db-prod");
        let index = all_matches_item(&items).expect("an all-matches item");
        let all = &items[index];
        assert_eq!(all.key_hint, Some(ALL_MATCHES_KEY));
        let PaletteCommand::Act(ObjectAction::Acknowledge, targets) = &all.command else {
            panic!("{:?}", all.command);
        };
        assert_eq!(
            all.label,
            format!("Acknowledge · all {} matches", targets.len())
        );
        // The single ones list what is named as typed first.
        assert!(
            items[..index]
                .iter()
                .take(targets.len().min(4))
                .all(|item| item.detail.contains("db-prod")),
            "{items:?}"
        );
        assert!(targets.len() > 1);
        // Every problem on a db-prod host, and nothing else.
        let snapshot = state.snapshot();
        let expected = snapshot
            .services
            .values()
            .filter(|service| service.key.host.as_str().contains("db-prod") && service.is_problem())
            .count()
            + snapshot
                .hosts
                .values()
                .filter(|host| host.name.as_str().contains("db-prod") && host.is_problem())
                .count();
        assert_eq!(targets.len(), expected);
        for target in targets {
            assert!(target.to_string().contains("db-prod"), "{target}");
        }
        // A scattered match never acts on everything.
        let fuzzy = PaletteIndex::build(&state, &Focus::default(), now()).search("ack dbprod");
        assert_eq!(all_matches_item(&fuzzy), None);
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
                .any(|item| item.command == PaletteCommand::Pause(PauseChoice::UntilMorning))
        );
        state.pause_notifications(Some(Timestamp::from_unix_seconds(1_790_003_600.)));
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
    fn each_environment_mutes_on_its_own() {
        let mut state = AppState::fixture(now());
        let mute = |state: &AppState| -> Vec<PaletteCommand> {
            PaletteIndex::build(state, &Focus::default(), now())
                .search("mute")
                .into_iter()
                .filter(|item| matches!(item.command, PaletteCommand::MuteEnvironment(..)))
                .map(|item| item.command)
                .collect()
        };
        assert!(
            mute(&state).is_empty(),
            "one environment: the pause is enough"
        );
        let staging = ic_config::Environment::new(
            "staging",
            "https://stg-master:5665",
            ic_config::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        let staging_id = staging.id.clone();
        state.save_environment(staging, false);
        assert!(mute(&state).contains(&PaletteCommand::MuteEnvironment(
            staging_id.clone(),
            Some(PauseChoice::Hour)
        )));
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("mute staging");
        assert!(
            items[0].label.starts_with("Mute staging "),
            "{}",
            items[0].label
        );
        assert_eq!(items[0].detail, "the other environments still notify");
        // Its name alone still switches to it.
        let items = PaletteIndex::build(&state, &Focus::default(), now()).search("staging");
        assert_eq!(
            items[0].command,
            PaletteCommand::SwitchEnvironment(staging_id.clone())
        );

        let later = Timestamp::from_unix_seconds(Timestamp::now().as_unix_seconds() + 3600.);
        assert!(state.pause_environment(&staging_id, Some(later)));
        let items = PaletteIndex::build(&state, &Focus::default(), Timestamp::now())
            .search("unmute staging");
        assert_eq!(items[0].label, "Unmute staging");
        assert_eq!(
            items[0].command,
            PaletteCommand::MuteEnvironment(staging_id, None)
        );
        assert!(items[0].detail.starts_with("muted until "));
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
