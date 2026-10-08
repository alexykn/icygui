//! What the command palette offers and finds (UI-03): commands (actions on
//! the focused or marked objects, reload, new dashboard or group, import
//! and export, pausing notifications, the sidebar), dashboards, hosts,
//! services, environments to switch to, and their settings. Pure, so it's
//! tested without a window.
//!
//! Every row has a mark in the dot's slot: an action on an object shows
//! the object's state dot (several objects: a stack in the worst state's
//! colour), a dashboard its summary dot, any other command a small icon.
//! An action on the focused object is an ordinary object-action row
//! (`Acknowledge · postgres-replication`, `on db-prod-03 · critical`);
//! the focus only ranks it first and gives it its key (`a`, `d`, `r`,
//! `c`), so the object is never listed twice.
//!
//! The candidates are indexed (lowercased) once when the palette opens, so
//! a keystroke over 30 000 services costs one pass of the matcher; only
//! the best few per section are kept.

use std::cmp::Reverse;

use ic_core::snapshot::Snapshot;
use ic_model::{Host, ObjectKey, Service, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::{IconName, ObjectMark};

use super::fuzzy::{Match, Query};
use crate::actions::ObjectAction;
use crate::app_state::AppState;
use crate::app_state::environments::url_summary;
use crate::lists::ListKind;
use crate::notifications::{MuteChoice, OverrideChange, PauseChoice};
use crate::settings::SettingsPage;
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
    /// Open the handling or downtimes view as a tab (topic 14), on a chip
    /// (*acknowledged* opens handling on its acknowledged chip).
    OpenList(ListKind, Option<crate::lists::model::Chip>),
    /// Mark every notification read.
    MarkNotificationsRead,
    /// Open the settings (`secondary-,`), on this tab.
    Settings(SettingsPage),
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
    /// It acts on several objects (a verb's *all N matches*, the marked
    /// rows): the dot's slot shows a several-objects mark instead, in
    /// `dot`'s colour (the worst state among them).
    pub(crate) several: bool,
    /// The mark in the dot's slot of a row without an object or a
    /// dashboard: a small muted icon that fits the command. Every row has
    /// a dot, a several-objects mark or an icon: the slot is never empty.
    pub(crate) icon: Option<IconName>,
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
    /// An action on the focused objects: ranks above equal matches by
    /// [`FOCUS_BONUS`].
    focus: bool,
}

/// How far a secondary command ranks below an equal match: more than a
/// label's length or start can make up.
const SECONDARY_PENALTY: i64 = 16;
/// How far an action on the focus ranks above an equal match (`mute`
/// lists muting the open object before muting an environment), not so far
/// that a scattered match beats a good one.
const FOCUS_BONUS: i64 = 8;

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
    /// The focused objects: an action on them is the same row however it
    /// was found.
    focus: Vec<ObjectKey>,
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
                    // Among the commands only the actions on the focus
                    // have an object's mark.
                    let focus = item.dot.is_some() || item.several;
                    Candidate {
                        secondary,
                        focus,
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
            focus: focus.targets.clone(),
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
                    // What the verb asked for comes first: its action on
                    // the focus (named or not), then on the objects named,
                    // then the commands matching the whole query, each
                    // object's action once.
                    commands.top = Some(i64::MAX);
                    let mut items = acts;
                    for item in std::mem::take(&mut commands.items) {
                        if !items.iter().any(|other| self.same_action(other, &item)) {
                            items.push(item);
                        }
                    }
                    if let Some(index) = items.iter().position(|item| {
                        self.runs(item).is_some_and(|(acting, targets)| {
                            *acting == action && !targets.is_empty() && targets == self.focus
                        })
                    }) {
                        let item = items.remove(index);
                        items.insert(0, item);
                    }
                    commands.items = items;
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
        // "Import dashboards". Ties keep the usual order.
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
                // The focus's is the row its key runs.
                let focused = self.focus == [key.clone()];
                Some(PaletteItem {
                    section: Section::Commands,
                    label: format!("{label} · {}", object.label),
                    detail: object.detail,
                    dot: object.dot,
                    several: false,
                    icon: None,
                    key_hint: if focused { action_key(action) } else { None },
                    command: PaletteCommand::Act(
                        action.clone(),
                        if focused { Vec::new() } else { vec![key] },
                    ),
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
            // The mark takes the worst state among them (the redder
            // unhandled state, as every dot; Icinga's severity among equals).
            let worst = services
                .iter()
                .chain(&hosts)
                .max_by_key(|candidate| (candidate.item.dot.map(Dot::rank), candidate.severity))
                .and_then(|candidate| candidate.item.dot);
            let objects = |candidates: Vec<&Candidate>| -> Vec<ObjectKey> {
                candidates
                    .into_iter()
                    .filter_map(|candidate| candidate.item.command.object().cloned())
                    .collect()
            };
            let detail = count_detail(services.len(), hosts.len());
            items.push(PaletteItem {
                section: Section::Commands,
                label: format!("{label} · all {count} matches"),
                detail,
                dot: worst,
                several: true,
                icon: None,
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

    /// The action `item` runs and the objects it runs on (the focused
    /// ones for an action on the focus), if it runs one.
    fn runs<'a>(&'a self, item: &'a PaletteItem) -> Option<(&'a ObjectAction, &'a [ObjectKey])> {
        match &item.command {
            PaletteCommand::Act(action, targets) if targets.is_empty() => {
                Some((action, self.focus.as_slice()))
            }
            PaletteCommand::Act(action, targets) => Some((action, targets.as_slice())),
            _ => None,
        }
    }

    /// Whether `a` and `b` run the same action on the same objects.
    fn same_action(&self, a: &PaletteItem, b: &PaletteItem) -> bool {
        matches!((self.runs(a), self.runs(b)), (Some(a), Some(b)) if a == b)
    }
}

/// `2 services · 1 host`: the objects an action on several runs on.
fn count_detail(services: usize, hosts: usize) -> String {
    [(services, "service"), (hosts, "host")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, noun)| format!("{count} {noun}{}", if count == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The key that runs `action` on the focus, if it has one.
fn action_key(action: &ObjectAction) -> Option<&'static str> {
    match action {
        ObjectAction::Acknowledge => Some("a"),
        ObjectAction::ScheduleDowntime => Some("d"),
        ObjectAction::CheckNow => Some("r"),
        ObjectAction::AddComment => Some("c"),
        _ => None,
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
                    let penalty = if candidate.secondary {
                        SECONDARY_PENALTY
                    } else {
                        0
                    };
                    let bonus = if candidate.focus { FOCUS_BONUS } else { 0 };
                    i64::from(found.score) - penalty + bonus
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
        focus: false,
    }
}

fn setting(label: String, detail: &str, icon: IconName, command: PaletteCommand) -> PaletteItem {
    PaletteItem {
        section: Section::Environments,
        label,
        detail: detail.to_owned(),
        dot: None,
        several: false,
        icon: Some(icon),
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
        ObjectAction::RemoveComments(names) if names.len() > 1 => "Remove comments",
        ObjectAction::RemoveComments(_) => "Remove comment",
        ObjectAction::RemoveNamedDowntimes(names) if names.len() > 1 => "Remove downtimes",
        ObjectAction::RemoveDowntime(_) | ObjectAction::RemoveNamedDowntimes(_) => {
            "Remove downtime"
        }
        ObjectAction::RemoveDowntimes => "Remove downtimes",
        ObjectAction::SubmitCheckResult => "Submit check result",
        ObjectAction::RunCommand => "Run command",
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
                    icon: None,
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
        .map(|host| object_candidate(host_item(host), host.severity(), host.is_problem()))
        .collect()
}

/// A host's row: its display name, then its name and address (when they
/// differ) and its state.
fn host_item(host: &Host) -> PaletteItem {
    let mark = ObjectMark::host(host);
    let state = mark.state;
    let address = if host.display_name == host.name.as_str() {
        host.address.clone()
    } else {
        format!("{} {}", host.name, host.address).trim().to_owned()
    };
    PaletteItem {
        section: Section::Hosts,
        label: host.display_name.clone(),
        detail: join_detail(&address, crate::format::state_word(state)),
        dot: Some(Dot::for_mark(mark)),
        several: false,
        icon: None,
        key_hint: None,
        command: PaletteCommand::OpenObject(host.key()),
        matched: Vec::new(),
        denied: None,
    }
}

/// Every service: display name, with its host and state after it.
fn service_candidates(state: &AppState) -> Vec<Candidate> {
    let snapshot = state.snapshot();
    snapshot
        .services
        .values()
        .map(|service| {
            object_candidate(
                service_item(snapshot, service),
                service.severity(),
                service.is_problem(),
            )
        })
        .collect()
}

/// A service's row: its display name, then `on <host> · <state>`.
fn service_item(snapshot: &Snapshot, service: &Service) -> PaletteItem {
    let host_object = snapshot.host_of(&service.key).map(AsRef::as_ref);
    let mark = ObjectMark::service(service, host_object);
    let state = mark.state;
    let host = host_object.map_or_else(
        || service.key.host.to_string(),
        |host| host.display_name.clone(),
    );
    PaletteItem {
        section: Section::Services,
        label: service.display_name.clone(),
        detail: join_detail(&format!("on {host}"), crate::format::state_word(state)),
        dot: Some(Dot::for_mark(mark)),
        several: false,
        icon: None,
        key_hint: None,
        command: PaletteCommand::OpenObject(service.object_key()),
        matched: Vec::new(),
        denied: None,
    }
}

/// What the actions on the focus name: the object as its row shows it
/// (`postgres-replication`, `on db-prod-03 · critical`, its state dot),
/// or the marked objects (`3 marked`, `2 services · 1 host`, the worst
/// state's stack).
struct Target {
    label: String,
    detail: String,
    dot: Dot,
    several: bool,
}

/// The [`Target`] of the actions on `focus` (`None`: nothing focused).
fn focus_target(state: &AppState, focus: &Focus) -> Option<Target> {
    let snapshot = state.snapshot();
    // The object's row, with its severity; one gone from the snapshot
    // still gets its name.
    let row = |key: &ObjectKey| -> (PaletteItem, u32) {
        let found = match key {
            ObjectKey::Host { name } => snapshot
                .hosts
                .get(name)
                .map(|host| (host_item(host), host.severity())),
            ObjectKey::Service { key } => snapshot
                .services
                .get(key)
                .map(|service| (service_item(snapshot, service), service.severity())),
        };
        found.unwrap_or_else(|| {
            let mut item = command(
                &crate::operate::forms::describe_objects(std::slice::from_ref(key)),
                String::new(),
                None,
                IconName::Layers,
                PaletteCommand::OpenObject(key.clone()),
            );
            item.dot = Some(Dot::for_object(None));
            (item, 0)
        })
    };
    match focus.targets.as_slice() {
        [] => None,
        [one] => {
            let (item, _) = row(one);
            Some(Target {
                label: item.label,
                detail: item.detail,
                dot: item.dot.unwrap_or(Dot::Empty),
                several: false,
            })
        }
        many => {
            let hosts = many
                .iter()
                .filter(|key| matches!(key, ObjectKey::Host { .. }))
                .count();
            let worst = many
                .iter()
                .map(row)
                .max_by_key(|(item, severity)| (item.dot.map(Dot::rank), *severity))
                .and_then(|(item, _)| item.dot)
                .unwrap_or(Dot::Empty);
            Some(Target {
                label: format!("{} marked", many.len()),
                detail: count_detail(many.len() - hosts, hosts),
                dot: worst,
                several: true,
            })
        }
    }
}

/// An action on the focus: `<verb> · <target>`, the target's detail and
/// mark, like an action on an object a query names.
fn target_item(
    verb: &str,
    target: &Target,
    key_hint: Option<&'static str>,
    command: PaletteCommand,
) -> PaletteItem {
    PaletteItem {
        section: Section::Commands,
        label: format!("{verb} · {}", target.label),
        detail: target.detail.clone(),
        dot: Some(target.dot),
        several: target.several,
        icon: None,
        key_hint,
        command,
        matched: Vec::new(),
        denied: None,
    }
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
                    icon: Some(IconName::ArrowLeftRight),
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
                format!("Edit environment {}", environment.name),
                "settings: URL, login, TLS",
                IconName::Settings,
                PaletteCommand::EditEnvironment(environment.id.clone()),
            ),
            0,
        ));
    }
    candidates.push(candidate(
        setting(
            "Add environment".to_owned(),
            "settings",
            IconName::Plus,
            PaletteCommand::AddEnvironment,
        ),
        0,
    ));
    candidates
}

/// A command item without an object: `icon` marks it.
fn command(
    label: &str,
    detail: String,
    key_hint: Option<&'static str>,
    icon: IconName,
    command: PaletteCommand,
) -> PaletteItem {
    PaletteItem {
        section: Section::Commands,
        label: label.to_owned(),
        detail,
        dot: None,
        several: false,
        icon: Some(icon),
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
    // A name away (`downtimes`), after what the empty query offers.
    if has_environment {
        items.extend(list_commands(state, now));
    }
    items.push(command(
        "Toggle sidebar",
        String::new(),
        Some(if cfg!(target_os = "macos") {
            "⌘B"
        } else {
            "ctrl-b"
        }),
        IconName::PanelLeft,
        PaletteCommand::ToggleSidebar,
    ));
    if has_environment {
        items.push(command(
            "Import dashboards",
            "from a file".to_owned(),
            None,
            IconName::FileInput,
            PaletteCommand::ImportDashboards,
        ));
        items.push(command(
            "Export dashboards",
            "every group, to a file".to_owned(),
            None,
            IconName::FileOutput,
            PaletteCommand::ExportDashboards,
        ));
    }
    items
}

/// The handling and downtimes views (topic 14), each opening as a tab:
/// `Handling  who is handling what · 20`; *acknowledged* and *comments*
/// open handling on that chip (there are no separate lists for them).
fn list_commands(state: &AppState, now: Timestamp) -> Vec<PaletteItem> {
    use crate::lists::model::Chip;
    let snapshot = state.snapshot();
    let count = |kind| {
        crate::lists::model::count(
            kind,
            snapshot,
            None,
            ic_config::DowntimeKinds::default(),
            now,
        )
    };
    let handled = count(ListKind::Handling);
    let in_effect = count(ListKind::Downtimes);
    vec![
        command(
            "Handling",
            format!("who is handling what · {handled}"),
            None,
            ListKind::Handling.icon(),
            PaletteCommand::OpenList(ListKind::Handling, Some(Chip::All)),
        ),
        command(
            "Downtimes",
            format!("in effect and upcoming · {in_effect} in effect"),
            None,
            ListKind::Downtimes.icon(),
            PaletteCommand::OpenList(ListKind::Downtimes, None),
        ),
        command(
            "Acknowledged",
            "handling, acknowledged problems".to_owned(),
            None,
            IconName::Check,
            PaletteCommand::OpenList(ListKind::Handling, Some(Chip::Acknowledged)),
        ),
        command(
            "Comments",
            "handling, comments".to_owned(),
            None,
            IconName::MessageSquare,
            PaletteCommand::OpenList(ListKind::Handling, Some(Chip::Comments)),
        ),
    ]
}

/// The actions on the focused objects, with their keys, then copying
/// their names and filter expression (PANE-05): rows like the actions on
/// an object a query names. Actions the API user may not run say why
/// (ENV-09).
fn action_commands(state: &AppState, focus: &Focus) -> (Vec<PaletteItem>, Vec<PaletteItem>) {
    let Some(target) = focus_target(state, focus) else {
        return (Vec::new(), Vec::new());
    };
    let snapshot = state.snapshot();
    let mut actions = vec![
        ObjectAction::Acknowledge,
        ObjectAction::ScheduleDowntime,
        ObjectAction::CheckNow,
        ObjectAction::AddComment,
        ObjectAction::SubmitCheckResult,
        ObjectAction::RunCommand,
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
        actions.push(ObjectAction::RemoveAcknowledgement);
    }
    // Only downtimes Icinga would remove: not those from the config.
    if focus.targets.iter().any(|target| {
        snapshot
            .downtimes
            .get(target)
            .is_some_and(|list| list.iter().any(|downtime| !downtime.config_owned))
    }) {
        actions.push(ObjectAction::RemoveDowntimes);
    }
    let mut items: Vec<PaletteItem> = actions
        .into_iter()
        .map(|action| {
            let denied = state.action_denial(&action);
            let mut item = target_item(
                action_label(&action),
                &target,
                action_key(&action),
                PaletteCommand::Act(action, Vec::new()),
            );
            item.denied = denied;
            item
        })
        .collect();
    let one = focus.targets.len() == 1;
    items.push(target_item(
        if one { "Copy name" } else { "Copy names" },
        &target,
        None,
        PaletteCommand::Copy {
            what: if one { "the name" } else { "the names" },
            text: crate::operate::expression::names(&focus.targets),
        },
    ));
    items.push(target_item(
        "Copy filter expression",
        &target,
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
            IconName::Refresh,
            PaletteCommand::Reload,
        ),
        command(
            "New dashboard",
            String::new(),
            Some(crate::sidebar::new_key()),
            IconName::Plus,
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
            IconName::Pencil,
            PaletteCommand::EditDashboard(selected.clone()),
        ));
    }
    items.push(command(
        "New group",
        String::new(),
        None,
        IconName::FolderPlus,
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
            IconName::Bell,
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
                    IconName::Pause,
                    PaletteCommand::Pause(choice),
                )
            })
            .collect(),
        None => Vec::new(),
    };
    items.extend(mute_commands(state, now));
    let unread = state.unread_notifications();
    items.push(command(
        "Notifications",
        if unread > 0 {
            format!("{unread} unread")
        } else {
            String::new()
        },
        None,
        IconName::Bell,
        PaletteCommand::OpenNotifications,
    ));
    if unread > 0 {
        items.push(command(
            "Mark all notifications read",
            String::new(),
            None,
            IconName::CheckCheck,
            PaletteCommand::MarkNotificationsRead,
        ));
    }
    if has_environment {
        items.push(command(
            "Notification settings",
            String::new(),
            None,
            IconName::Settings,
            PaletteCommand::Settings(SettingsPage::Notifications),
        ));
    }
    items.push(command(
        "Settings",
        String::new(),
        Some(crate::settings::settings_key()),
        IconName::Settings,
        PaletteCommand::Settings(SettingsPage::General),
    ));
    items.push(command(
        "About icygui",
        String::new(),
        None,
        IconName::Info,
        PaletteCommand::About,
    ));
    items.push(command(
        "Quit icygui",
        String::new(),
        Some(crate::settings::quit_key()),
        IconName::Power,
        PaletteCommand::Quit,
    ));
    items
}

/// Muting each environment on its own, or unmuting it (A5), with
/// several environments: the one on screen first (the empty query's few
/// commands offer its mute), then the others in their order (found by
/// typing their name).
fn mute_commands(state: &AppState, now: Timestamp) -> Vec<PaletteItem> {
    let environments = state.environments();
    let mut items = Vec::new();
    if environments.len() < 2 {
        return items;
    }
    let (on_screen, others): (Vec<_>, Vec<_>) = environments
        .iter()
        .partition(|environment| state.is_active(&environment.id));
    for environment in on_screen.into_iter().chain(others) {
        let id = &environment.id;
        match state.environment_paused_until(id, now) {
            Some(until) => items.push(command(
                &format!("Unmute {}", environment.name),
                format!("muted until {}", crate::notifications::when(until, now)),
                None,
                IconName::Bell,
                PaletteCommand::MuteEnvironment(id.clone(), None),
            )),
            None => items.extend(PauseChoice::ALL.into_iter().map(|choice| {
                command(
                    &format!("Mute {} {}", environment.name, choice.label(now)),
                    "the other environments still notify".to_owned(),
                    None,
                    IconName::BellOff,
                    PaletteCommand::MuteEnvironment(id.clone(), Some(choice)),
                )
            })),
        }
    }
    items
}

/// Watching, muting and unmuting the focused objects (NOTE-02), as
/// actions on them.
fn override_commands(state: &AppState, focus: &Focus, now: Timestamp) -> Vec<PaletteItem> {
    if state.environment().is_none() {
        return Vec::new();
    }
    let Some(target) = focus_target(state, focus) else {
        return Vec::new();
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
        items.push(target_item(
            "Watch",
            &target,
            None,
            PaletteCommand::Override(OverrideChange::Watch, targets.clone()),
        ));
    }
    for choice in MuteChoice::ALL {
        items.push(target_item(
            &format!("Mute {}", choice.label(now)),
            &target,
            None,
            PaletteCommand::Override(OverrideChange::Mute(choice), targets.clone()),
        ));
    }
    if any {
        items.push(target_item(
            if all_watched {
                "Stop watching"
            } else {
                "Unmute"
            },
            &target,
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
        assert!(items.iter().any(|item| item.label == "Add environment"));
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
        // scattered one like "Import dashboards" (i-m-P-o-R-t … O … D).
        assert_ne!(items[0].label, "Import dashboards");
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
        assert_eq!(items[0].label, "Import dashboards");
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

    /// The focused object's actions are ordinary object-action rows: its
    /// name and detail, its state dot; the focus only puts them first and
    /// gives them their keys, so a query naming it lists it once.
    #[test]
    fn actions_on_the_focus_come_first() {
        let replication = ObjectKey::service("db-prod-03", "postgres-replication");
        let focus = Focus {
            targets: vec![replication.clone()],
        };
        let palette = index(&focus);
        let items = palette.search("");
        assert_eq!(items[0].label, "Acknowledge · postgres-replication");
        assert_eq!(items[0].detail, "on db-prod-03 · critical");
        assert!(
            matches!(items[0].dot, Some(Dot::State(_))) && !items[0].several,
            "{:?}",
            items[0].dot
        );
        assert_eq!(items[0].key_hint, Some("a"));
        assert_eq!(
            items[0].command,
            PaletteCommand::Act(ObjectAction::Acknowledge, Vec::new())
        );
        assert_eq!(
            labels(&items[..4]),
            [
                "Acknowledge · postgres-replication",
                "Schedule downtime · postgres-replication",
                "Check now · postgres-replication",
                "Add comment · postgres-replication",
            ]
        );

        // Named by a verb query: first, once, with its key.
        let items = palette.search("ack postgres");
        assert_eq!(items[0].label, "Acknowledge · postgres-replication");
        assert_eq!(items[0].key_hint, Some("a"));
        let acknowledging = |item: &&PaletteItem| {
            palette.runs(item)
                == Some((
                    &ObjectAction::Acknowledge,
                    std::slice::from_ref(&replication),
                ))
        };
        assert_eq!(
            items.iter().filter(acknowledging).count(),
            1,
            "{:?}",
            labels(&items)
        );
        assert!(
            items[1..]
                .iter()
                .all(|item| matches!(item.key_hint, None | Some(ALL_MATCHES_KEY))),
            "only the focus has the action's key"
        );
        // A query the focus doesn't match still runs on the focus first.
        let check = palette.search("check now");
        assert_eq!(check[0].label, "Check now · postgres-replication");
        assert_eq!(check[0].key_hint, Some("r"));

        // Marked rows: their count and the worst state's stack.
        let marked = index(&Focus {
            targets: vec![replication, ObjectKey::host("db-prod-03")],
        })
        .search("");
        assert_eq!(marked[0].label, "Acknowledge · 2 marked");
        assert_eq!(marked[0].detail, "1 service · 1 host");
        assert!(marked[0].several && marked[0].dot.is_some());
    }

    /// Every row has a mark in the dot's slot, names objects as their rows
    /// do (never `host!service`), and no label ends in `…`.
    #[test]
    fn every_row_has_a_mark() {
        let mut state = AppState::fixture(now());
        let staging = ic_config::Environment::new(
            "staging",
            "https://staging:5665",
            ic_config::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        state.save_environment(staging, false);
        for targets in [
            Vec::new(),
            vec![ObjectKey::service("db-prod-03", "postgres-replication")],
            vec![ObjectKey::host("db-prod-03"), ObjectKey::host("db-prod-01")],
        ] {
            let palette = PaletteIndex::build(&state, &Focus { targets }, now());
            for query in [
                "",
                "o",
                "ack db-prod",
                "mute",
                "settings",
                "copy",
                "staging",
            ] {
                for item in palette.search(query) {
                    assert!(
                        item.dot.is_some() || item.several || item.icon.is_some(),
                        "{query}: {item:?}"
                    );
                    assert!(!item.label.contains('!'), "{query}: {}", item.label);
                    assert!(!item.label.ends_with('…'), "{query}: {}", item.label);
                }
            }
        }
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

    /// The empty query's few commands offer muting the environment on
    /// screen, not the first in the list; the others are a name away.
    #[test]
    fn the_empty_query_offers_muting_the_environment_on_screen() {
        let mut state = AppState::fixture(now());
        let prod_id = state.active_environment_id().unwrap().to_owned();
        let staging = ic_config::Environment::new(
            "staging",
            "https://stg-master:5665",
            ic_config::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        let staging_id = staging.id.clone();
        state.save_environment(staging, false);
        let muted = |state: &AppState, query: &str| -> Vec<String> {
            PaletteIndex::build(state, &Focus::default(), now())
                .search(query)
                .into_iter()
                .filter_map(|item| match item.command {
                    PaletteCommand::MuteEnvironment(id, _) => Some(id),
                    _ => None,
                })
                .collect()
        };
        for active in [&prod_id, &staging_id] {
            state.switch_environment(active);
            let offered = muted(&state, "");
            assert!(!offered.is_empty(), "offered");
            assert!(offered.iter().all(|id| id == active), "{offered:?}");
        }
        assert!(muted(&state, "mute prod").contains(&prod_id), "a name away");
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
