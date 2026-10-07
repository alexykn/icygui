//! A dashboard's views (v1, topic 04): what each one shows and how.
//!
//! A dashboard is a list of views stacked on one page. Each view has its
//! own display (a list, a grouped list, a host-group grid, summary tiles,
//! an event stream, and (topic 14) handling or downtimes), filter and
//! options. A dashboard from rc1 has exactly one view, a list or grouped
//! list, and looks as it did.
//!
//! Handled problems are hidden or shown per kind ([`HideHandled`]): the
//! defaults are in the settings (`[appearance.hide_handled]`), and every
//! list view follows them unless it sets its own ([`HandledSetting`]).
//!
//! Every control of a view's header is kept with the view (topic 14,
//! round 5): the sort, a list's state chip, the handled switch, the
//! handling and downtimes views' chip, sort and mode
//! ([`ThreadOptions`]), *only mine* and the row density. *Only mine* and
//! the density are personal: an export leaves them out
//! ([`View::personal_cleared`]).

use serde::{Deserialize, Serialize};

use crate::model::RowDensity;

/// The most views a dashboard may have. Each view is evaluated on every
/// change, so a dashboard with dozens of them would cost as much as dozens
/// of dashboards.
pub const MAX_VIEWS: usize = 16;

/// The fewest and most lines an event stream view may ask for.
pub const STREAM_LINES: std::ops::RangeInclusive<u32> = 1..=200;

/// One view of a dashboard.
///
/// Every field has a default, so a hand-written view needs only what
/// differs (`filter = "…"` alone is a list of service problems).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct View {
    /// Stable id, unique within its dashboard: the UI matches a view's
    /// evaluated result to it by this id. Blank ids are filled in when the
    /// settings load ([`crate::Config::repair_ids`]).
    pub id: String,
    /// The view's name, shown on its header (`failing services`). A
    /// dashboard with one view shows the dashboard's name instead, so an
    /// rc1 dashboard's only view has none.
    pub name: String,
    /// How the view shows what it matches.
    pub display: ViewDisplay,
    /// Hosts or services: what a list, grouped list or summary tiles view
    /// lists and counts. A host-group grid always shows hosts (with their
    /// services' states, [`GridColour`]); an event stream follows the
    /// hosts and services its filter matches.
    pub object_kind: ObjectKind,
    /// Icinga filter expression; empty = everything.
    pub filter: String,
    /// Only objects in a problem state (lists and grouped lists).
    pub problems_only: bool,
    /// Which handled problems a list or grouped list hides.
    pub handled: HandledSetting,
    /// Sort order of a list's rows.
    pub sort: Sort,
    /// What a grouped list groups by ([`GroupBy::None`] there means by
    /// host). A plain list ignores it, so switching between the two keeps
    /// the choice.
    pub group_by: GroupBy,
    /// The view starts collapsed to its header ("collapse by default").
    pub collapsed: bool,
    /// The groups a host-group grid or summary tiles view shows.
    #[serde(skip_serializing_if = "is_default")]
    pub groups: ViewGroups,
    /// A host-group grid's options.
    #[serde(skip_serializing_if = "is_default")]
    pub grid: GridOptions,
    /// An event stream's options.
    #[serde(skip_serializing_if = "is_default")]
    pub stream: StreamOptions,
    /// The handling and downtimes views' options: the chip they open with,
    /// their sort, the downtimes view's timeline or list, which downtimes
    /// it shows, *only mine*.
    #[serde(skip_serializing_if = "is_default")]
    pub threads: ThreadOptions,
    /// A list's state chip: the header's per-state count picked, so the
    /// list shows only objects in that state (`None`: every state).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<StateChip>,
    /// How tall the rows of a list-like view are ([`ViewDisplay::has_rows`]);
    /// `None` follows the settings (`[appearance] row_density`). Personal:
    /// never exported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<RowDensity>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            display: ViewDisplay::List,
            object_kind: ObjectKind::Services,
            filter: String::new(),
            problems_only: true,
            handled: HandledSetting::default(),
            sort: Sort::default(),
            group_by: GroupBy::None,
            collapsed: false,
            groups: ViewGroups::default(),
            grid: GridOptions::default(),
            stream: StreamOptions::default(),
            threads: ThreadOptions::default(),
            state: None,
            density: None,
        }
    }
}

impl View {
    /// What a list's rows are grouped by: nothing for a plain list, the
    /// view's [`View::group_by`] for a grouped list (by host when that is
    /// [`GroupBy::None`]). Other displays have no list rows.
    #[must_use]
    pub fn list_grouping(&self) -> GroupBy {
        match self.display {
            ViewDisplay::GroupedList if self.group_by == GroupBy::None => GroupBy::Host,
            ViewDisplay::GroupedList => self.group_by,
            _ => GroupBy::None,
        }
    }

    /// Groups a list's rows (or stops grouping them with
    /// [`GroupBy::None`]): a list becomes a grouped list and back. Other
    /// displays only keep the choice.
    pub fn set_grouping(&mut self, group_by: GroupBy) {
        self.group_by = group_by;
        match self.display {
            ViewDisplay::List | ViewDisplay::GroupedList => {
                self.display = if group_by == GroupBy::None {
                    ViewDisplay::List
                } else {
                    ViewDisplay::GroupedList
                };
            }
            _ => {}
        }
    }

    /// Whether the view lists objects as rows (a list or grouped list).
    #[must_use]
    pub fn is_list(&self) -> bool {
        matches!(self.display, ViewDisplay::List | ViewDisplay::GroupedList)
    }

    /// Whether the view's objects count toward the dashboard's sidebar
    /// count and dot and its notifications ([`ViewDisplay::counts_problems`]).
    #[must_use]
    pub fn counts_problems(&self) -> bool {
        self.display.counts_problems()
    }

    /// The view without what is personal (*only mine*, the row density):
    /// what an export carries, and what an imported view starts from.
    #[must_use]
    pub fn personal_cleared(&self) -> Self {
        let mut view = self.clone();
        view.density = None;
        view.threads.only_mine = false;
        view
    }

    /// The view as far as its evaluation goes: without what only the
    /// app's drawing reads (the row density, the handling and downtimes
    /// views' options). A view that differs from another only there
    /// evaluates to the same result.
    #[must_use]
    pub fn evaluated(&self) -> Self {
        let mut view = self.clone();
        view.density = None;
        view.threads = ThreadOptions::default();
        view
    }

    /// The handled problems the view hides, given the settings' defaults.
    /// Only lists and grouped lists hide any; the other displays show
    /// handled objects hollow.
    #[must_use]
    pub fn hidden_handled(&self, defaults: HideHandled) -> HideHandled {
        if self.is_list() {
            self.handled.hidden(defaults)
        } else {
            HideHandled::NONE
        }
    }
}

/// How a view shows what it matches (the editor's *display*).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewDisplay {
    /// One row per object.
    #[default]
    List,
    /// Rows under a header per host or group ([`View::group_by`]).
    GroupedList,
    /// A square (or labelled cell) per host, grouped by host group or a
    /// custom variable's value.
    HostGroupGrid,
    /// A tile per host group (or custom variable value) with its counts.
    SummaryTiles,
    /// The latest events (state changes, acknowledgements, downtimes,
    /// comments, flapping) of the objects the filter matches, from the
    /// local event log.
    EventStream,
    /// Who is handling what (topic 14): a thread per object the filter
    /// matches with its acknowledgement, its downtimes and its comments.
    Handling,
    /// The downtimes of the objects the filter matches, in effect and to
    /// come, as a timeline or a list (topic 14).
    Downtimes,
}

impl ViewDisplay {
    /// Every display, in the order of the editor's *add view* menu: the
    /// lists, the overviews, then the activity (which never counts).
    pub const ALL: [Self; 7] = [
        Self::List,
        Self::GroupedList,
        Self::HostGroupGrid,
        Self::SummaryTiles,
        Self::EventStream,
        Self::Handling,
        Self::Downtimes,
    ];

    /// Whether a view of this display counts toward its dashboard's
    /// sidebar count and dot and its notifications: the problem views
    /// (lists, grids, tiles), whose objects have states. An event stream,
    /// handling and downtimes show activity: they never count and never
    /// notify (topic 04).
    #[must_use]
    pub fn counts_problems(self) -> bool {
        matches!(
            self,
            Self::List | Self::GroupedList | Self::HostGroupGrid | Self::SummaryTiles
        )
    }

    /// Whether the view has rows whose density can be chosen: lists,
    /// grouped lists, event streams, handling and downtimes (topic 14,
    /// round 5); grids and tiles have none.
    #[must_use]
    pub fn has_rows(self) -> bool {
        !matches!(self, Self::HostGroupGrid | Self::SummaryTiles)
    }

    /// Whether the view shows topic 14's threads: handling or downtimes.
    #[must_use]
    pub fn is_threads(self) -> bool {
        matches!(self, Self::Handling | Self::Downtimes)
    }
}

/// A list's state chip: the per-state count picked in its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateChip {
    /// Critical services.
    Critical,
    /// Services in warning.
    Warning,
    /// Unknown services.
    Unknown,
    /// Hosts that are down.
    Down,
    /// Unreachable hosts.
    Unreachable,
}

/// The handling and downtimes views' options (topic 14): what their
/// headers show, kept with the view (round 5: every header control is
/// remembered). The editor's *opens with* is [`ThreadOptions::chip`], its
/// *opens as* [`ThreadOptions::mode`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ThreadOptions {
    /// The chip picked (*opens with*).
    pub chip: ThreadChip,
    /// The sort chosen; `None`: the chip's (or the mode's) own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sort: Option<ThreadSort>,
    /// The downtimes view's display (*opens as*).
    pub mode: DowntimesMode,
    /// Which downtimes the downtimes view shows (*shows*).
    #[serde(skip_serializing_if = "is_default")]
    pub shows: DowntimeKinds,
    /// Only what the environment's author set. Personal: never exported.
    pub only_mine: bool,
}

/// A chip of a handling or downtimes view: what it shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadChip {
    /// Everything.
    #[default]
    All,
    /// Acknowledged problems (handling).
    Acknowledged,
    /// Downtimes in effect.
    InEffect,
    /// Downtimes not in effect yet.
    Upcoming,
    /// Free-standing comments (handling).
    Comments,
    /// Downtimes from the config (downtimes).
    FromConfig,
}

/// How a handling or downtimes view is sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadSort {
    /// The newest thread first (handling).
    LatestActivity,
    /// What comes back or changes soonest first.
    Soonest,
    /// By when the downtimes start (the timeline's).
    ByTime,
    /// By object.
    Object,
    /// By who set it.
    Author,
}

/// The downtimes view's display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DowntimesMode {
    /// Bars on a shared axis around now (the default).
    #[default]
    Timeline,
    /// Sections and groups.
    List,
}

/// Which downtimes a downtimes view shows (all by default).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct DowntimeKinds {
    /// Downtimes in effect.
    pub in_effect: bool,
    /// Downtimes not in effect yet.
    pub upcoming: bool,
    /// Downtimes from the config (`ScheduledDowntime`).
    pub from_config: bool,
}

impl Default for DowntimeKinds {
    fn default() -> Self {
        Self {
            in_effect: true,
            upcoming: true,
            from_config: true,
        }
    }
}

/// Hosts or services.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectKind {
    /// Services.
    #[default]
    Services,
    /// Hosts.
    Hosts,
}

/// Sort order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct Sort {
    /// Sort key.
    pub key: SortKey,
    /// Largest / newest first.
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Severity,
            descending: true,
        }
    }
}

/// Sort keys. Ties fall back to severity, then name.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    /// Icinga severity (the design's default, `severity ↓`).
    #[default]
    Severity,
    /// Time of the last state change.
    LastStateChange,
    /// Host name.
    Host,
    /// Service name (host name for host views).
    Service,
}

/// Grouping of list rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    /// Flat list.
    #[default]
    None,
    /// By host.
    Host,
    /// By host group (an object in several groups appears in each).
    HostGroup,
    /// By service group.
    ServiceGroup,
}

/// The kinds of handled problems that are hidden: Icinga's *handled* in
/// its three parts. In the settings (`[appearance.hide_handled]`) they are
/// the defaults of every list view; a view may set its own
/// ([`HandledSetting`]).
///
/// A problem that is handled for several reasons (acknowledged, and its
/// host is down) is hidden when any of its reasons is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct HideHandled {
    /// Problems someone has acknowledged.
    pub acknowledged: bool,
    /// Hosts and services whose downtime is in effect (whatever their
    /// state: an OK service in downtime counts as handled too).
    pub in_downtime: bool,
    /// Services of hosts that are down or unreachable: the host's own
    /// problem covers them; the host still shows.
    pub host_down: bool,
}

impl Default for HideHandled {
    fn default() -> Self {
        Self::ALL
    }
}

impl HideHandled {
    /// Hide every kind (the settings' default).
    pub const ALL: Self = Self {
        acknowledged: true,
        in_downtime: true,
        host_down: true,
    };

    /// Hide nothing: every handled problem shows, hollow.
    pub const NONE: Self = Self {
        acknowledged: false,
        in_downtime: false,
        host_down: false,
    };

    /// Whether any kind is hidden.
    #[must_use]
    pub fn any(self) -> bool {
        self.acknowledged || self.in_downtime || self.host_down
    }
}

/// Which handled problems a list view hides (the editor's *handled*
/// field, and the view header's `N hidden · show` button).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct HandledSetting {
    /// Follow the settings, show every handled problem, or hide the kinds
    /// in [`HandledSetting::hide`].
    pub mode: HandledMode,
    /// The kinds hidden with [`HandledMode::Hide`]; kept with the other
    /// modes, so switching back restores them.
    #[serde(skip_serializing_if = "is_default")]
    pub hide: HideHandled,
}

impl HandledSetting {
    /// Follow the settings' defaults.
    pub const SETTINGS: Self = Self {
        mode: HandledMode::Settings,
        hide: HideHandled::ALL,
    };

    /// Show every handled problem.
    pub const SHOW: Self = Self {
        mode: HandledMode::Show,
        hide: HideHandled::ALL,
    };

    /// The kinds hidden, given the settings' defaults.
    #[must_use]
    pub fn hidden(self, defaults: HideHandled) -> HideHandled {
        match self.mode {
            HandledMode::Settings => defaults,
            HandledMode::Show => HideHandled::NONE,
            HandledMode::Hide => self.hide,
        }
    }

    /// The setting after a click on the view's handled button: `show`
    /// shows every handled problem; `hide` goes back to what hid them
    /// before (the view's own kinds when it had chosen some, else the
    /// settings), and when that would hide nothing (the settings hide no
    /// kind), hides the view's own kinds, else every kind: `hide` always
    /// hides.
    #[must_use]
    pub fn toggled(self, defaults: HideHandled) -> Self {
        if self.hidden(defaults).any() {
            Self {
                mode: HandledMode::Show,
                hide: self.hide,
            }
        } else if self.mode == HandledMode::Show && self.hide.any() && self.hide != HideHandled::ALL
        {
            Self {
                mode: HandledMode::Hide,
                hide: self.hide,
            }
        } else if defaults.any() {
            Self {
                mode: HandledMode::Settings,
                hide: self.hide,
            }
        } else {
            Self {
                mode: HandledMode::Hide,
                hide: if self.hide.any() {
                    self.hide
                } else {
                    HideHandled::ALL
                },
            }
        }
    }
}

/// How a view decides which handled problems to hide.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HandledMode {
    /// As the settings say (`[appearance.hide_handled]`).
    #[default]
    Settings,
    /// Every handled problem shows, hollow.
    Show,
    /// The view hides the kinds it names.
    Hide,
}

/// The groups a host-group grid or summary tiles view shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewGroups {
    /// Host groups, or the values of a host custom variable.
    pub by: GroupSource,
    /// The host groups shown, by name or glob pattern (`pg-*`); empty:
    /// every host group.
    pub host_groups: Vec<String>,
    /// The host custom variable whose values are the groups (`site`;
    /// `host.vars.site` works too). A host whose value is an array is in a
    /// group per element.
    pub custom_var: String,
    /// The order of the groups.
    pub order: GroupOrder,
}

impl ViewGroups {
    /// The custom variable's name without a `host.vars.` or `vars.`
    /// prefix, trimmed.
    #[must_use]
    pub fn custom_var_name(&self) -> &str {
        let name = self.custom_var.trim();
        name.strip_prefix("host.vars.")
            .or_else(|| name.strip_prefix("vars."))
            .unwrap_or(name)
    }
}

/// What a grid's or tiles view's groups are.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupSource {
    /// Host groups.
    #[default]
    HostGroup,
    /// The values of a host custom variable ([`ViewGroups::custom_var`]).
    CustomVar,
}

/// The order of a grid's or tiles view's groups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroupOrder {
    /// The group with the worst unhandled problem first (then by name).
    #[default]
    WorstFirst,
    /// By name.
    Name,
}

/// A host-group grid's options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct GridOptions {
    /// What a host's square shows.
    pub colour: GridColour,
    /// Squares or labelled cells.
    pub cells: GridCells,
    /// Leave out groups where every host is OK.
    pub hide_healthy_groups: bool,
    /// A host in several groups shows in each (else only in the first).
    pub host_in_each_group: bool,
}

impl Default for GridOptions {
    fn default() -> Self {
        Self {
            colour: GridColour::default(),
            cells: GridCells::default(),
            hide_healthy_groups: false,
            host_in_each_group: true,
        }
    }
}

/// What a grid's square shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridColour {
    /// The worst of the host's state and its services' (the default).
    #[default]
    WorstOfHostAndServices,
    /// The host's own state.
    HostOnly,
}

/// How a grid draws its hosts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GridCells {
    /// A 12px square per host (the default).
    #[default]
    Squares,
    /// A cell per host with its state dot and name.
    LabelledCells,
}

/// An event stream's options.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamOptions {
    /// Which kinds of events it shows.
    pub events: StreamEvents,
    /// Only hard state changes (soft ones are left out).
    pub hard_states_only: bool,
    /// Also recoveries: state changes to OK or UP.
    pub recoveries: bool,
    /// How many lines show before the stream scrolls
    /// ([`STREAM_LINES`]).
    pub lines: u32,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            events: StreamEvents::default(),
            hard_states_only: true,
            recoveries: false,
            lines: 8,
        }
    }
}

/// The kinds of events an event stream shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent switches of the settings file, one key each"
)]
pub struct StreamEvents {
    /// State changes.
    pub state_changes: bool,
    /// Acknowledgements set and cleared.
    pub acknowledgements: bool,
    /// Downtimes starting and ending.
    pub downtimes: bool,
    /// Comments added and removed.
    pub comments: bool,
    /// Flapping starting and stopping.
    pub flapping: bool,
}

impl Default for StreamEvents {
    fn default() -> Self {
        Self {
            state_changes: true,
            acknowledgements: true,
            downtimes: true,
            comments: true,
            flapping: false,
        }
    }
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_follows_the_display() {
        let mut view = View::default();
        assert_eq!(view.list_grouping(), GroupBy::None);
        view.set_grouping(GroupBy::HostGroup);
        assert_eq!(view.display, ViewDisplay::GroupedList);
        assert_eq!(view.list_grouping(), GroupBy::HostGroup);
        view.set_grouping(GroupBy::None);
        assert_eq!(view.display, ViewDisplay::List);
        // A grouped list without a grouping groups by host.
        view.display = ViewDisplay::GroupedList;
        assert_eq!(view.list_grouping(), GroupBy::Host);
        // Other displays keep the choice but have no rows to group.
        view.display = ViewDisplay::SummaryTiles;
        view.set_grouping(GroupBy::Host);
        assert_eq!(view.display, ViewDisplay::SummaryTiles);
        assert_eq!(view.list_grouping(), GroupBy::None);
    }

    #[test]
    fn handled_kinds_follow_the_settings_unless_set_on_the_view() {
        let defaults = HideHandled {
            in_downtime: false,
            ..HideHandled::ALL
        };
        assert_eq!(HandledSetting::SETTINGS.hidden(defaults), defaults);
        assert_eq!(HandledSetting::SHOW.hidden(defaults), HideHandled::NONE);
        let own = HandledSetting {
            mode: HandledMode::Hide,
            hide: HideHandled {
                acknowledged: true,
                ..HideHandled::NONE
            },
        };
        assert_eq!(own.hidden(defaults), own.hide);
        // Only list views hide anything.
        let mut view = View {
            handled: own,
            ..View::default()
        };
        assert_eq!(view.hidden_handled(defaults), own.hide);
        view.display = ViewDisplay::HostGroupGrid;
        assert_eq!(view.hidden_handled(defaults), HideHandled::NONE);
    }

    #[test]
    fn the_handled_button_toggles_show_and_back() {
        let all = HideHandled::ALL;
        let shown = HandledSetting::SETTINGS.toggled(all);
        assert_eq!(shown.mode, HandledMode::Show);
        assert_eq!(shown.toggled(all), HandledSetting::SETTINGS);
        // A view's own kinds come back after show.
        let own = HandledSetting {
            mode: HandledMode::Hide,
            hide: HideHandled {
                host_down: false,
                ..HideHandled::ALL
            },
        };
        assert_eq!(own.toggled(all).mode, HandledMode::Show);
        assert_eq!(own.toggled(all).toggled(all), own);
        // Settings that hide nothing: `N handled · hide` still hides, every
        // kind (the view's own kinds when it has some), and show comes back.
        let none = HideHandled::NONE;
        let hidden = HandledSetting::SETTINGS.toggled(none);
        assert_eq!(hidden.mode, HandledMode::Hide);
        assert_eq!(hidden.hidden(none), HideHandled::ALL);
        assert_eq!(hidden.toggled(none).mode, HandledMode::Show);
        assert_eq!(hidden.toggled(none).toggled(none).hidden(none), all);
        let shown = HandledSetting {
            mode: HandledMode::Show,
            hide: HideHandled::NONE,
        };
        assert_eq!(shown.toggled(all).mode, HandledMode::Settings);
        assert_eq!(shown.toggled(none).hidden(none), all);
    }

    #[test]
    fn custom_var_names_lose_their_prefix() {
        for (written, name) in [
            ("site", "site"),
            (" host.vars.site ", "site"),
            ("vars.rack", "rack"),
            ("", ""),
        ] {
            let groups = ViewGroups {
                custom_var: written.to_owned(),
                ..ViewGroups::default()
            };
            assert_eq!(groups.custom_var_name(), name, "{written:?}");
        }
    }

    #[test]
    fn only_problem_views_count() {
        for display in ViewDisplay::ALL {
            let view = View {
                display,
                ..View::default()
            };
            let activity = matches!(
                display,
                ViewDisplay::EventStream | ViewDisplay::Handling | ViewDisplay::Downtimes
            );
            assert_eq!(view.counts_problems(), !activity, "{display:?}");
            assert_eq!(
                display.has_rows(),
                !matches!(
                    display,
                    ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles
                ),
                "{display:?}"
            );
        }
    }

    #[test]
    fn personal_choices_stay_home() {
        let view = View {
            density: Some(RowDensity::Compact),
            threads: ThreadOptions {
                only_mine: true,
                chip: ThreadChip::Upcoming,
                ..ThreadOptions::default()
            },
            ..View::default()
        };
        let shared = view.personal_cleared();
        assert_eq!(shared.density, None);
        assert!(!shared.threads.only_mine);
        // The rest is shared: the chip it opens with, say.
        assert_eq!(shared.threads.chip, ThreadChip::Upcoming);
        // Drawing choices don't change an evaluation.
        assert_eq!(view.evaluated(), View::default());
    }

    #[test]
    fn thread_options_round_trip_in_toml() {
        let view = View {
            display: ViewDisplay::Downtimes,
            threads: ThreadOptions {
                chip: ThreadChip::FromConfig,
                sort: Some(ThreadSort::Author),
                mode: DowntimesMode::List,
                shows: DowntimeKinds {
                    from_config: false,
                    ..DowntimeKinds::default()
                },
                only_mine: true,
            },
            state: Some(StateChip::Warning),
            density: Some(RowDensity::Compact),
            ..View::default()
        };
        let text = toml::to_string(&view).unwrap();
        assert!(text.contains("display = \"downtimes\""), "{text}");
        assert!(text.contains("chip = \"from_config\""), "{text}");
        assert!(text.contains("mode = \"list\""), "{text}");
        assert!(text.contains("density = \"compact\""), "{text}");
        let back: View = toml::from_str(&text).unwrap();
        assert_eq!(back, view);
        // A plain view writes none of it.
        let plain = toml::to_string(&View::default()).unwrap();
        assert!(!plain.contains("threads"), "{plain}");
        assert!(!plain.contains("density"), "{plain}");
        assert!(!plain.contains("state"), "{plain}");
    }
}
