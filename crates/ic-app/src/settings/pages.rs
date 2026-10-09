//! The settings panel's pages, drawn from mock-up 02: one block per
//! section (a label over its rows), each row a name, a line of
//! description and a control that applies at once; and the search, which
//! lists the matching rows of every page under `category · section`
//! headings, working in place.
//!
//! What the rows read from the app is gathered once per frame
//! ([`Facts`]); the controls call the panel's `change_*` methods, which
//! write the settings straight away.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, Context, ElementId, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, Styled as _, div, prelude::FluentBuilder as _,
};
use ic_config::{Appearance, General, InterfaceSize, ListTimes, LogLevel, RowDensity, ThemeChoice};
use ic_core::snapshot::Summary;
use ic_model::{CheckableState, ObjectKey, Timestamp};
use ic_rules::{ObjectMode, Rule, ScopeSetting};
use ic_ui_kit::{
    Button, Chip, Icon, IconButton, IconName, ListRow, Menu, MenuItem, Segmented, Select,
    StateCircle, StateDot, Switch, TextField, Theme, Tooltip, px,
};

use super::files;
use super::keyboard::{Activate, Region, Step};
use super::model::{
    RowText, SCOPE_CHOICES, Section, Setting, contains, scope_choice, scope_meaning,
    section_matches,
};
use super::rows::{self, Row, SUB_INDENT};
use super::{
    FIXED_RECONCILE_DEFAULT, FieldId, RuleFlag, ScopeKey, SettingsEvent, SettingsMenu,
    SettingsPage, SettingsPanel,
};
use crate::app_state::connection::{ConnectionStatus, Health};
use crate::app_state::{AppState, EngineSlot, NotificationPlan};
use crate::home::tilde;
use crate::keymap::{ShortcutRow, bindings_in_effect, shortcut_rows};
use crate::menu_state::down_position;
use crate::notifications::{PauseChoice, override_text, pause_label, paused_text};

/// What choosing option `index` of a segmented control does.
type Choose = Rc<dyn Fn(&mut SettingsPanel, usize, &mut gpui::Window, &mut Context<SettingsPanel>)>;

/// The weekdays, Monday first, as quiet hours' day chips show them.
const DAYS: [&str; 7] = ["mo", "tu", "we", "th", "fr", "sa", "su"];

/// What a framed field or dropdown adds to the width the frames give
/// (their content's): 10px of padding and a 1px border each side.
const FIELD_FRAME: f32 = 22.;
/// The widths of the fields and dropdowns, as the frames give them.
const ENVIRONMENT_DROPDOWN: f32 = 180.;
const LOG_LEVEL_DROPDOWN: f32 = 120.;
const MIN_DURATION_FIELD: f32 = 80.;
const TIME_FIELD: f32 = 64.;
const STORM_FIELD: f32 = 44.;
const RECONCILE_FIELD: f32 = 80.;
const RETENTION_FIELD: f32 = 64.;

/// The keymap table's keys column.
const KEYS_COLUMN: f32 = 220.;
/// The keymap table's "where" column.
const PLACE_COLUMN: f32 = 150.;
/// Space between the keymap table's columns.
const COLUMN_GAP: f32 = 24.;
/// How many rows the appearance preview shows.
const PREVIEW_ROWS: usize = 3;

/// An environment as the icinga page and the environment dropdown show
/// it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EnvironmentFact {
    /// Its id.
    pub(crate) id: String,
    /// Its name.
    pub(crate) name: String,
    /// Its URLs' hosts (`master-01, master-02`), which a search finds it
    /// by besides its name.
    pub(crate) hosts: String,
    /// Its connection's health (the dot).
    pub(crate) health: Health,
    /// `master-01, master-02 · 2 URLs · connected to master-01`.
    pub(crate) summary: String,
}

/// A watched or muted object of the page's environment.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OverrideFact {
    /// The object.
    pub(crate) object: ObjectKey,
    /// `postgres-replication on db-prod-03`.
    pub(crate) label: String,
    /// `watched · always notifies`, `muted until 08:00`.
    pub(crate) text: String,
    /// Watched (else muted).
    pub(crate) watched: bool,
    /// Its state, if the environment's snapshot has it.
    pub(crate) state: Option<CheckableState>,
}

/// A row of the appearance preview.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PreviewRow {
    state: CheckableState,
    handled: bool,
    name: String,
    host: Option<String>,
    output: String,
    /// How long in this state (`14m`).
    relative: String,
    /// Since when (`13:58`).
    clock: String,
}

/// What the rows read from the app, once per frame.
pub(crate) struct Facts {
    pub(crate) now: Timestamp,
    pub(crate) general: General,
    pub(crate) appearance: Appearance,
    /// The page's environment's notification settings.
    pub(crate) plan: Option<NotificationPlan>,
    /// The page's environment's name.
    pub(crate) environment_name: Option<String>,
    pub(crate) environments: Vec<EnvironmentFact>,
    /// The pause of every environment, if one runs.
    pub(crate) paused_until: Option<Timestamp>,
    pub(crate) overrides: Vec<OverrideFact>,
    /// The page's environment's dashboards' counts, for their dots.
    pub(crate) dashboard_summaries: HashMap<(String, String), Summary>,
    /// The shortcuts in effect (read only for the keymap page and the
    /// search).
    pub(crate) shortcuts: Vec<ShortcutRow>,
    /// How many key bindings are in effect.
    pub(crate) shortcut_count: usize,
    /// What the keymap file couldn't bind.
    pub(crate) keymap_problems: Vec<String>,
    /// The preview's dashboard and rows.
    pub(crate) preview: (Option<String>, Vec<PreviewRow>),
}

impl Facts {
    /// Reads what `panel` shows.
    pub(crate) fn read(panel: &SettingsPanel, cx: &App) -> Self {
        let state = panel.state.read(cx);
        let now = Timestamp::now();
        let environment = panel.environment.as_deref();
        let plan = environment.and_then(|id| state.notification_plan_of(id));
        let snapshot = environment.and_then(|id| state.snapshot_of(id));
        let overrides = plan
            .as_ref()
            .map(|plan| {
                plan.settings
                    .objects
                    .iter()
                    .filter(|entry| entry.until.is_none_or(|until| until > now))
                    .map(|entry| OverrideFact {
                        object: entry.object.clone(),
                        label: crate::operate::forms::describe_objects(std::slice::from_ref(
                            &entry.object,
                        )),
                        text: override_text(entry, now),
                        watched: entry.mode == ObjectMode::Watch,
                        state: snapshot.and_then(|snapshot| {
                            crate::operate::dialog::object_state(snapshot, &entry.object)
                        }),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let dashboard_summaries = snapshot
            .map(|snapshot| {
                snapshot
                    .dashboards
                    .iter()
                    .map(|(reference, result)| {
                        (
                            (reference.group_id.clone(), reference.dashboard_id.clone()),
                            result.summary,
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let keymap_needed = panel.page == SettingsPage::Keymap || !panel.query.is_empty();
        let (shortcuts, shortcut_count) = if keymap_needed {
            let bound = bindings_in_effect(cx);
            (shortcut_rows(&bound), bound.len())
        } else {
            (Vec::new(), 0)
        };
        let preview = if panel.page == SettingsPage::Appearance && panel.query.is_empty() {
            preview_rows(state, now)
        } else {
            (None, Vec::new())
        };
        Self {
            now,
            general: state.config().general.clone(),
            appearance: *state.appearance(),
            environment_name: environment
                .and_then(|id| state.environment_by_id(id))
                .map(|environment| environment.name.clone()),
            environments: state
                .environments()
                .iter()
                .map(|environment| EnvironmentFact {
                    id: environment.id.clone(),
                    name: environment.name.clone(),
                    hosts: url_hosts(environment).join(", "),
                    health: state
                        .slot(&environment.id)
                        .map_or(Health::Connecting, |slot| slot.connection().health(now)),
                    summary: environment_summary(
                        environment,
                        state.slot(&environment.id).map(EngineSlot::connection),
                        now,
                    ),
                })
                .collect(),
            paused_until: state.paused_until().filter(|until| *until > now),
            plan,
            overrides,
            dashboard_summaries,
            shortcuts,
            shortcut_count,
            keymap_problems: crate::keymap::user_keymap(cx)
                .map(|keymap| keymap.problems.clone())
                .unwrap_or_default(),
            preview,
        }
    }
}

/// An environment in a line: its URLs' hosts, how many URLs, and what its
/// connection does (`master-01, master-02 · 2 URLs · connected to
/// master-01`).
fn environment_summary(
    environment: &ic_config::Environment,
    connection: Option<&ConnectionStatus>,
    now: Timestamp,
) -> String {
    let hosts = url_hosts(environment);
    let urls = match environment.urls.len() {
        1 => "1 URL".to_owned(),
        count => format!("{count} URLs"),
    };
    let state = match connection {
        None => "starting".to_owned(),
        Some(connection) => connection_words(connection, &hosts, now),
    };
    let mut parts = Vec::new();
    if !hosts.is_empty() {
        parts.push(hosts.join(", "));
    }
    parts.push(urls);
    parts.push(state);
    parts.join(" · ")
}

/// The hosts of an environment's URLs, each once, in order.
fn url_hosts(environment: &ic_config::Environment) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for host in environment
        .urls
        .iter()
        .filter_map(|url| url.api_url().ok())
        .filter_map(|url| url.host_str().map(str::to_owned))
    {
        if !hosts.contains(&host) {
            hosts.push(host);
        }
    }
    hosts
}

/// What a connection does, in a few words: `connected to master-01`,
/// `stale, last event 41s ago`, `login refused`.
fn connection_words(connection: &ConnectionStatus, hosts: &[String], now: Timestamp) -> String {
    match connection.health(now) {
        Health::Live => {
            let quiet = if connection.is_quiet() {
                " · quiet"
            } else {
                ""
            };
            let node = connection.endpoint.as_str();
            // One URL whose host is the node: naming it again says nothing.
            if node.is_empty() || (hosts.len() == 1 && hosts[0] == node) {
                format!("connected{quiet}")
            } else {
                format!("connected to {node}{quiet}")
            }
        }
        Health::Stale => match connection.label_parts(now).1 {
            Some(age) => format!("stale, last event {age} ago"),
            None => "stale".to_owned(),
        },
        Health::Idle => "not started".to_owned(),
        Health::Reconnecting | Health::Failed | Health::Connecting => {
            connection.short_state().to_owned()
        }
    }
}

/// The selected dashboard's first rows for the appearance preview, with
/// its name; none while nothing is loaded.
fn preview_rows(state: &AppState, now: Timestamp) -> (Option<String>, Vec<PreviewRow>) {
    let Some(reference) = state.selected() else {
        return (None, Vec::new());
    };
    let Some((_, dashboard)) = state.dashboard(reference) else {
        return (None, Vec::new());
    };
    let name = Some(dashboard.name.clone());
    let snapshot = state.snapshot();
    let Some(result) = snapshot
        .dashboards
        .get(reference)
        .and_then(|result| crate::dashboard::primary_result(result, &dashboard.views))
    else {
        return (name, Vec::new());
    };
    let rows = result
        .rows()
        .iter()
        .filter_map(|row| match row {
            ic_core::snapshot::DashboardRow::Object(key) => Some(key),
            ic_core::snapshot::DashboardRow::Group { .. } => None,
        })
        .filter_map(|key| {
            let row = crate::dashboard::rows::object_row(snapshot, key, now)?;
            Some(PreviewRow {
                state: row.state,
                handled: row.handled,
                name: row.name,
                host: row.host,
                output: row.output,
                relative: row.since,
                clock: row.clock,
            })
        })
        .take(PREVIEW_ROWS)
        .collect();
    (name, rows)
}

/// A row of data a section lists besides its settings.
#[derive(Clone, Debug, PartialEq, Eq)]
enum DataItem {
    /// A watched or muted object, by index in [`Facts::overrides`].
    Override(usize),
    /// A group, by index in the plan.
    Group(usize),
    /// A dashboard: group index, dashboard index.
    Dashboard(usize, usize),
    /// An environment, by index in [`Facts::environments`].
    Environment(usize),
    /// A shortcut, by index in [`Facts::shortcuts`].
    Shortcut(usize),
}

/// A stable element id prefix for a scope.
fn scope_id(key: &ScopeKey) -> String {
    match key {
        ScopeKey::Environment => "rule-environment".to_owned(),
        ScopeKey::Group(id) => format!("rule-group-{id}"),
        ScopeKey::Dashboard(group, id) => format!("rule-dashboard-{group}-{id}"),
    }
}

/// The dot of a dashboard's counts: the worst unhandled state, green when
/// objects match and nothing is unhandled, grey otherwise.
fn summary_color(summary: Option<&Summary>, theme: &Theme) -> gpui::Hsla {
    let Some(summary) = summary else {
        return theme.states.fill.pending;
    };
    if let Some(state) = summary.worst_unhandled {
        return theme.states.fill.checkable(state);
    }
    let checked = summary.ok
        + summary.critical
        + summary.warning
        + summary.unknown
        + summary.down
        + summary.unreachable;
    if checked > 0 {
        theme.states.fill.ok
    } else {
        theme.states.fill.pending
    }
}

/// A 7px dot in a 14px slot (every list row's mark).
fn mark_dot(color: gpui::Hsla) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .w(px(14.))
        .justify_center()
        .child(StateDot::with_color(color).size(px(7.)))
        .into_any_element()
}

impl SettingsPanel {
    // --- Content --------------------------------------------------------

    /// The page's blocks (one per section) and their sections, or the
    /// search's.
    pub(super) fn render_content(
        &mut self,
        theme: &Theme,
        facts: &Facts,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, Vec<Section>) {
        if !self.query.is_empty() {
            return self.render_search(theme, facts, cx);
        }
        let sections = self.page.sections();
        let blocks = sections
            .iter()
            .map(|section| self.render_section(*section, theme, facts, cx))
            .collect();
        (blocks, sections.to_vec())
    }

    /// A section of the page shown: its label, its settings, its data.
    fn render_section(
        &self,
        section: Section,
        theme: &Theme,
        facts: &Facts,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut block = div().flex().flex_col();
        if section != Section::Shortcuts {
            block = block.child(rows::section_label(
                section.label(),
                Self::section_note(section, facts).as_deref(),
                false,
                "",
                theme,
            ));
        }
        if section == Section::ThisEnvironment && facts.plan.is_none() {
            block = block.child(rows::info_note(
                "Add an environment first: notification rules belong to an environment.",
                theme,
            ));
        }
        for setting in Setting::ALL {
            if setting.section() == section && Self::is_shown(setting, facts) {
                block = block.child(self.render_setting(setting, theme, facts, cx));
                if setting == Setting::Reconcile && facts.general.reconcile_interval_secs == 0 {
                    block = block.child(
                        div()
                            .pb(px(12.))
                            .mt(px(-4.))
                            .text_size(theme.text.small)
                            .line_height(gpui::relative(1.5))
                            .text_color(theme.colors.text_muted)
                            .child(
                                "Adaptive: every 5 minutes for a small Icinga, every 15 at \
                                 30 000 objects, up to an hour while the stream runs without \
                                 a break.",
                            ),
                    );
                }
            }
        }
        match section {
            Section::Background => {
                block = block.child(rows::info_note(
                    "The tray icon shows the worst unhandled state; its menu opens the window, \
                     pauses notifications, switches environments and quits. Notifications are \
                     as prompt in quiet mode; outputs catch up when you look.",
                    theme,
                ));
            }
            Section::Preview => block = block.child(Self::render_preview(theme, facts)),
            Section::Shortcuts => block = block.children(self.render_keymap(theme, facts, cx)),
            Section::WatchedAndMuted if facts.plan.is_some() && facts.overrides.is_empty() => {
                block = block.child(rows::info_note(
                    "Nothing is watched or muted. Watch or mute a host or service from its \
                     pane’s ··· menu or the palette.",
                    theme,
                ));
            }
            Section::GroupsAndDashboards
                if facts
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.groups.is_empty()) =>
            {
                block = block.child(rows::info_note("No dashboards yet.", theme));
            }
            _ => {}
        }
        let items = self.data_items(section, facts, "");
        if section != Section::Shortcuts {
            block = block.children(
                items
                    .iter()
                    .flat_map(|item| self.render_item(item, true, theme, facts, cx)),
            );
        }
        if section == Section::Environments {
            block = block.child(
                div()
                    .flex()
                    .py(px(12.))
                    .border_t_1()
                    .border_color(theme.colors.border_row)
                    .child(
                        self.button(
                            "settings-add-environment",
                            false,
                            Button::new("settings-add-environment", "add environment")
                                .icon(IconName::Plus),
                            Rc::new(|_, _, cx| cx.emit(SettingsEvent::AddEnvironment)),
                            Region::Page,
                            theme,
                            cx,
                        ),
                    ),
            );
        }
        block.into_any_element()
    }

    /// What follows a section's label.
    fn section_note(section: Section, facts: &Facts) -> Option<String> {
        match section {
            Section::Preview => Some(match &facts.preview {
                (Some(name), rows) if !rows.is_empty() => {
                    format!("the {name} dashboard as it would look")
                }
                _ => "rows as they would look".to_owned(),
            }),
            _ => section.note().map(str::to_owned),
        }
    }

    /// The search: the matching rows of every page, under `category ·
    /// section` headings, then the pages without a match.
    fn render_search(
        &self,
        theme: &Theme,
        facts: &Facts,
        cx: &Context<Self>,
    ) -> (Vec<AnyElement>, Vec<Section>) {
        let mut blocks = Vec::new();
        let mut sections = Vec::new();
        let mut empty = Vec::new();
        for page in SettingsPage::ALL {
            let (matches, count) = self.page_matches(page, facts, cx);
            if count == 0 {
                empty.push(page.label());
                continue;
            }
            for section in page.sections() {
                let section = *section;
                let settings: Vec<(Setting, bool)> = matches
                    .settings
                    .iter()
                    .copied()
                    .filter(|(setting, _)| setting.section() == section)
                    .collect();
                let items = self.data_items(section, facts, &self.query);
                if settings.is_empty() && items.is_empty() {
                    continue;
                }
                let heading = if page.sections().len() == 1 {
                    page.label().to_owned()
                } else {
                    format!("{} · {}", page.label(), section.label())
                };
                let mut block = div().flex().flex_col().child(rows::section_label(
                    &heading,
                    None,
                    true,
                    &self.query,
                    theme,
                ));
                for (setting, _) in settings {
                    block = block.child(self.render_setting(setting, theme, facts, cx));
                }
                if section == Section::Shortcuts {
                    let shortcuts: Vec<&ShortcutRow> = items
                        .iter()
                        .filter_map(|item| match item {
                            DataItem::Shortcut(index) => facts.shortcuts.get(*index),
                            _ => None,
                        })
                        .collect();
                    block = block.children(self.keymap_rows(&shortcuts, theme));
                } else {
                    block = block.children(
                        items
                            .iter()
                            .flat_map(|item| self.render_item(item, false, theme, facts, cx)),
                    );
                }
                blocks.push(block.into_any_element());
                sections.push(section);
            }
        }
        let note = if blocks.is_empty() {
            format!(
                "no setting matches “{}”",
                self.search.read(cx).value().trim()
            )
        } else if empty.is_empty() {
            String::new()
        } else {
            format!("no matches in {}", empty.join(", "))
        };
        if !note.is_empty() {
            blocks.push(
                div()
                    .pt(px(2.))
                    .child(rows::section_label(&note, None, false, "", theme))
                    .into_any_element(),
            );
        }
        (blocks, sections)
    }

    // --- Settings -------------------------------------------------------

    /// The name and description of a setting's row.
    #[expect(clippy::too_many_lines, reason = "one arm per setting, each its words")]
    pub(super) fn row_text(&self, setting: Setting, facts: &Facts) -> RowText {
        let text = |name: &str, description: &str| RowText {
            name: name.to_owned(),
            description: description.to_owned(),
        };
        match setting {
            Setting::CloseToTray => text(
                "keep running in the tray",
                if self.tray_host == Some(false) {
                    "No tray on this desktop (GNOME needs the AppIndicator extension): closing \
                     quits icygui."
                } else {
                    "When the window closes, icygui stays in the tray and keeps notifying."
                },
            ),
            Setting::LaunchAtLogin => text(
                "start at login",
                if self.demo {
                    "Not in the demo: it would start the demo at every login."
                } else {
                    "Starts icygui in the tray, without its window, when you log in."
                },
            ),
            Setting::QuietMode => text(
                "quiet mode when hidden",
                "Out of sight for half a minute: follow Icinga without check results. Far less \
                 load on the master.",
            ),
            Setting::Theme => text(
                "theme",
                "Follow system switches with your desktop’s light or dark mode.",
            ),
            Setting::InterfaceSize => text(
                "interface size",
                "Scales text and spacing in every window: 90 %, 100 % or 115 %.",
            ),
            Setting::RowDensity => text(
                "row density",
                "Compact drops the output line: one line per object, about twice the rows.",
            ),
            Setting::ListTimes => text(
                "times in lists",
                "Under the state circle: how long in this state (14m), or since when (13:58).",
            ),
            Setting::HideAcknowledged => {
                text("hide acknowledged", "Problems someone has acknowledged.")
            }
            Setting::HideInDowntime => text(
                "hide in downtime",
                "Hosts and services whose downtime is in effect.",
            ),
            Setting::HideHostDown => text(
                "hide services of hosts that are down",
                "The host’s own problem covers them; the host still shows.",
            ),
            Setting::Environment => text(
                "environment",
                "Notification rules belong to an environment; each one notifies on its own.",
            ),
            Setting::Enabled => RowText {
                name: match &facts.environment_name {
                    Some(name) => format!("notifications for {name}"),
                    None => "notifications".to_owned(),
                },
                description: "Desktop notifications from this environment’s rules.".to_owned(),
            },
            Setting::PauseAll => {
                let count = facts.environments.len();
                text(
                    pause_label(count),
                    if count > 1 {
                        "Every environment, until it runs out; the tray and the centre can \
                         resume."
                    } else {
                        "Until it runs out; the tray and the centre can resume."
                    },
                )
            }
            // App-wide on the page of one environment: it says so.
            Setting::PluginOutput => text(
                "show plugin output",
                "Its first line in every environment’s notifications. Off for shared screens \
                 and the lock screen.",
            ),
            Setting::States => text("states", "Recoveries follow problems that notified."),
            Setting::Events => text("events", "Also notify when these start or end."),
            Setting::HardOnly => text("hard states only", "Soft states are retries in progress."),
            Setting::SkipHandled => text(
                "skip handled problems",
                "Acknowledged, in downtime, or the host is down.",
            ),
            Setting::MinDuration => text(
                "only after",
                "A problem must last this long first (5m, 1h; 0 = at once).",
            ),
            Setting::Sound => text(
                "play a sound",
                "The system’s alert sound, by state where the desktop plays sounds.",
            ),
            Setting::QuietHours => text(
                "record silently at night",
                "Notifications in the window go to the centre without a desktop notification.",
            ),
            Setting::QuietTimes => text(
                "from and to",
                "May cross midnight; the days are the ones it starts on.",
            ),
            Setting::QuietDays => text("days", ""),
            Setting::QuietLoud => text(
                "critical and down still notify out loud",
                "During quiet hours.",
            ),
            Setting::Storm => text("storm threshold", "Beyond it, one summary notification."),
            Setting::Reconcile => text(
                "reconcile with Icinga",
                "A lean reload of every object catches what the event stream missed.",
            ),
            Setting::ReconcileInterval => text(
                "every",
                "At least 60 seconds: a lean reload of a large Icinga costs the master memory.",
            ),
            Setting::Retention => text(
                "keep events for",
                "The history tabs and the notification centre read the local log.",
            ),
            Setting::LogLevel => text(
                "log level",
                "What icygui writes to its log; debug adds each request’s path and timing.",
            ),
            Setting::LogFolder => RowText {
                name: "log folder".to_owned(),
                description: match &self.locations.log_dir {
                    Some(dir) => match self.log_summary {
                        Some(summary) => format!(
                            "{} · {} of at most {}",
                            tilde(dir),
                            summary.text(),
                            files::size_text(crate::logging::MAX_TOTAL_BYTES)
                        ),
                        None => tilde(dir),
                    },
                    None => "No log file: icygui logs to the terminal only.".to_owned(),
                },
            },
            Setting::ConfigFolder => RowText {
                name: "config folder".to_owned(),
                description: format!(
                    "{} · config.toml (settings and dashboards), keymap.toml",
                    self.locations
                        .config_dir
                        .as_deref()
                        .map_or_else(|| "not on disk".to_owned(), tilde)
                ),
            },
            Setting::About => RowText {
                name: format!("icygui {}", env!("CARGO_PKG_VERSION")),
                description: "IBM Plex Mono, Lucide icons, GPUI; licences in the about dialog."
                    .to_owned(),
            },
        }
    }

    /// Whether a setting's row is shown: notification rows need an
    /// environment, rows under a switch need it on.
    pub(super) fn is_shown(setting: Setting, facts: &Facts) -> bool {
        let plan = facts.plan.as_ref();
        match setting {
            Setting::Environment
            | Setting::Enabled
            | Setting::PauseAll
            | Setting::States
            | Setting::Events
            | Setting::HardOnly
            | Setting::SkipHandled
            | Setting::MinDuration
            | Setting::Sound
            | Setting::QuietHours
            | Setting::Storm => plan.is_some(),
            Setting::QuietTimes | Setting::QuietDays | Setting::QuietLoud => {
                plan.is_some_and(|plan| plan.settings.quiet_hours.enabled)
            }
            Setting::ReconcileInterval => facts.general.reconcile_interval_secs != 0,
            _ => true,
        }
    }

    /// The fields whose problems show under a setting's row.
    fn fields_of(setting: Setting) -> &'static [FieldId] {
        match setting {
            Setting::MinDuration => &[FieldId::MinDuration(ScopeKey::Environment)],
            Setting::QuietTimes => &[FieldId::QuietStart, FieldId::QuietEnd],
            Setting::Storm => &[FieldId::StormThreshold, FieldId::StormWindow],
            Setting::ReconcileInterval => &[FieldId::Reconcile],
            Setting::Retention => &[FieldId::Retention],
            _ => &[],
        }
    }

    /// A setting's row, its matches marked in a search (a row found by its
    /// section's name has the match in its heading, not on itself).
    fn render_setting(
        &self,
        setting: Setting,
        theme: &Theme,
        facts: &Facts,
        cx: &Context<Self>,
    ) -> AnyElement {
        let text = self.row_text(setting, facts);
        let query = self.query.as_str();
        let name = rows::marked(&text.name, query, theme);
        let description =
            (!text.description.is_empty()).then(|| rows::marked(&text.description, query, theme));
        let error = Self::fields_of(setting)
            .iter()
            .find_map(|field| self.errors.get(field).cloned());
        Row::new(name)
            .description(description)
            .error(error)
            .indent(if setting.is_sub() { SUB_INDENT } else { 0. })
            .control(self.control(setting, theme, facts, cx))
            .render(theme)
    }

    /// A setting's control, reachable with Tab (see `keyboard`).
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per setting, each a few lines"
    )]
    fn control(
        &self,
        setting: Setting,
        theme: &Theme,
        facts: &Facts,
        cx: &Context<Self>,
    ) -> AnyElement {
        let general = &facts.general;
        let appearance = facts.appearance;
        let rule = facts.plan.as_ref().map(|plan| &plan.settings.default_rule);
        let quiet = facts.plan.as_ref().map(|plan| plan.settings.quiet_hours);
        match setting {
            Setting::CloseToTray => self.switch(
                "settings-close-to-tray",
                general.close_to_tray,
                false,
                |this, on, _, cx| this.change_general(cx, |general| general.close_to_tray = on),
                theme,
                cx,
            ),
            Setting::LaunchAtLogin => self.switch(
                "settings-launch-at-login",
                general.launch_at_login,
                self.demo,
                |this, on, _, cx| this.change_general(cx, |general| general.launch_at_login = on),
                theme,
                cx,
            ),
            Setting::QuietMode => self.switch(
                "settings-quiet-mode",
                general.quiet_when_hidden,
                false,
                |this, on, _, cx| {
                    this.change_general(cx, |general| general.quiet_when_hidden = on);
                },
                theme,
                cx,
            ),
            Setting::Theme => self.segmented(
                "settings-theme",
                &["follow system", "dark", "light"],
                match appearance.theme {
                    ThemeChoice::System => 0,
                    ThemeChoice::Dark => 1,
                    ThemeChoice::Light => 2,
                },
                theme,
                cx,
                |this, index, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.theme = match index {
                            1 => ThemeChoice::Dark,
                            2 => ThemeChoice::Light,
                            _ => ThemeChoice::System,
                        };
                    });
                },
            ),
            Setting::InterfaceSize => self.segmented(
                "settings-interface-size",
                &["small", "default", "large"],
                match appearance.interface_size {
                    InterfaceSize::Small => 0,
                    InterfaceSize::Default => 1,
                    InterfaceSize::Large => 2,
                },
                theme,
                cx,
                |this, index, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.interface_size = match index {
                            0 => InterfaceSize::Small,
                            2 => InterfaceSize::Large,
                            _ => InterfaceSize::Default,
                        };
                    });
                },
            ),
            Setting::RowDensity => self.segmented(
                "settings-row-density",
                &["comfortable", "compact"],
                usize::from(appearance.row_density == RowDensity::Compact),
                theme,
                cx,
                |this, index, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.row_density = if index == 1 {
                            RowDensity::Compact
                        } else {
                            RowDensity::Comfortable
                        };
                    });
                },
            ),
            Setting::ListTimes => self.segmented(
                "settings-list-times",
                &["relative", "clock"],
                usize::from(appearance.list_times == ListTimes::Clock),
                theme,
                cx,
                |this, index, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.list_times = if index == 1 {
                            ListTimes::Clock
                        } else {
                            ListTimes::Relative
                        };
                    });
                },
            ),
            Setting::HideAcknowledged => self.switch(
                "settings-hide-acknowledged",
                appearance.hide_handled.acknowledged,
                false,
                |this, on, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.hide_handled.acknowledged = on;
                    });
                },
                theme,
                cx,
            ),
            Setting::HideInDowntime => self.switch(
                "settings-hide-in-downtime",
                appearance.hide_handled.in_downtime,
                false,
                |this, on, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.hide_handled.in_downtime = on;
                    });
                },
                theme,
                cx,
            ),
            Setting::HideHostDown => self.switch(
                "settings-hide-host-down",
                appearance.hide_handled.host_down,
                false,
                |this, on, _, cx| {
                    this.change_appearance(cx, |appearance| {
                        appearance.hide_handled.host_down = on;
                    });
                },
                theme,
                cx,
            ),
            Setting::Environment => {
                let ids: Vec<String> = facts
                    .environments
                    .iter()
                    .map(|environment| environment.id.clone())
                    .collect();
                let current = self.environment.clone();
                // The page's environment's health dot, as on its row.
                let dot = facts
                    .environments
                    .iter()
                    .find(|environment| current.as_deref() == Some(environment.id.as_str()))
                    .map(|environment| crate::sidebar::health_color(environment.health, theme));
                self.dropdown(
                    "settings-environment",
                    SettingsMenu::Environment,
                    facts.environment_name.clone().unwrap_or_default(),
                    dot,
                    ENVIRONMENT_DROPDOWN,
                    self.environment_menu(theme, facts, cx),
                    Rc::new(move |this, forward, window, cx| {
                        if let Some(id) = neighbour(&ids, current.as_ref(), forward) {
                            this.choose_environment(&id, window, cx);
                        }
                    }),
                    theme,
                    cx,
                )
            }
            Setting::Enabled => self.switch(
                "settings-notifications-enabled",
                facts
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.settings.enabled),
                false,
                |this, on, _, cx| this.change_plan(cx, |plan| plan.settings.enabled = on),
                theme,
                cx,
            ),
            Setting::PauseAll => self.pause_control(theme, facts, cx),
            Setting::PluginOutput => self.switch(
                "settings-plugin-output",
                general.show_plugin_output,
                false,
                |this, on, _, cx| {
                    this.change_general(cx, |general| general.show_plugin_output = on);
                },
                theme,
                cx,
            ),
            Setting::States => rule.map_or_else(empty, |rule| {
                self.flag_chips(&ScopeKey::Environment, &RuleFlag::STATES, rule, theme, cx)
            }),
            Setting::Events => rule.map_or_else(empty, |rule| {
                self.flag_chips(&ScopeKey::Environment, &RuleFlag::EVENTS, rule, theme, cx)
            }),
            Setting::HardOnly | Setting::SkipHandled | Setting::Sound => {
                let flag = match setting {
                    Setting::HardOnly => RuleFlag::HardOnly,
                    Setting::SkipHandled => RuleFlag::SkipHandled,
                    _ => RuleFlag::Sound,
                };
                rule.map_or_else(empty, |rule| {
                    self.flag_switch(&ScopeKey::Environment, flag, rule, theme, cx)
                })
            }
            Setting::MinDuration => self.field(
                &FieldId::MinDuration(ScopeKey::Environment),
                MIN_DURATION_FIELD,
                theme,
                cx,
            ),
            Setting::QuietHours => self.switch(
                "settings-quiet",
                quiet.is_some_and(|quiet| quiet.enabled),
                false,
                |this, on, _, cx| this.set_quiet_hours(on, cx),
                theme,
                cx,
            ),
            Setting::QuietTimes => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(self.field(&FieldId::QuietStart, TIME_FIELD, theme, cx))
                .child(
                    div()
                        .text_size(theme.text.small)
                        .text_color(theme.colors.text_faint)
                        .child("→"),
                )
                .child(self.field(&FieldId::QuietEnd, TIME_FIELD, theme, cx))
                .into_any_element(),
            Setting::QuietDays => {
                let days = quiet.map(|quiet| quiet.days).unwrap_or_default();
                div()
                    .flex()
                    .gap(px(6.))
                    .children(DAYS.iter().enumerate().map(|(index, day)| {
                        let on = days[index];
                        let id = SharedString::from(format!("settings-day-{day}"));
                        let toggle: Activate = Rc::new(move |this, _, cx| {
                            this.change_plan(cx, |plan| {
                                plan.settings.quiet_hours.days[index] = !on;
                            });
                        });
                        let click = toggle.clone();
                        self.focusable(
                            id.clone(),
                            Chip::new(id, *day).selected(on).on_click(cx.listener(
                                move |this, _: &ClickEvent, window, cx| click(this, window, cx),
                            )),
                            toggle,
                            None,
                            theme,
                            cx,
                        )
                    }))
                    .into_any_element()
            }
            Setting::QuietLoud => self.switch(
                "settings-quiet-critical",
                quiet.is_some_and(|quiet| quiet.allow_critical),
                false,
                |this, on, _, cx| {
                    this.change_plan(cx, |plan| plan.settings.quiet_hours.allow_critical = on);
                },
                theme,
                cx,
            ),
            Setting::Storm => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(rows::words("at most", theme))
                .child(self.field(&FieldId::StormThreshold, STORM_FIELD, theme, cx))
                .child(rows::words("in", theme))
                .child(self.field(&FieldId::StormWindow, STORM_FIELD, theme, cx))
                .child(rows::words("seconds", theme))
                .into_any_element(),
            Setting::Reconcile => self.segmented(
                "settings-reconcile",
                &["adaptive", "fixed interval"],
                usize::from(general.reconcile_interval_secs != 0),
                theme,
                cx,
                |this, index, window, cx| {
                    this.change_general(cx, |general| {
                        general.reconcile_interval_secs =
                            match (index, general.reconcile_interval_secs) {
                                (0, _) => 0,
                                (_, 0) => FIXED_RECONCILE_DEFAULT,
                                (_, seconds) => seconds,
                            };
                    });
                    // The interval's field shows what is in effect now.
                    this.errors.remove(&FieldId::Reconcile);
                    this.show_stored(&FieldId::Reconcile, window, cx);
                },
            ),
            Setting::ReconcileInterval => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(self.field(&FieldId::Reconcile, RECONCILE_FIELD, theme, cx))
                .child(rows::words("seconds", theme))
                .into_any_element(),
            Setting::Retention => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(self.field(&FieldId::Retention, RETENTION_FIELD, theme, cx))
                .child(rows::words("hours", theme))
                .into_any_element(),
            Setting::LogLevel => {
                let level = general.log_level;
                self.dropdown(
                    "settings-log-level",
                    SettingsMenu::LogLevel,
                    level.as_str().to_owned(),
                    None,
                    LOG_LEVEL_DROPDOWN,
                    Self::log_level_menu(facts, cx),
                    Rc::new(move |this, forward, _, cx| {
                        if let Some(next) = neighbour(&LogLevel::ALL, Some(&level), forward) {
                            this.change_general(cx, |general| general.log_level = next);
                        }
                    }),
                    theme,
                    cx,
                )
            }
            Setting::LogFolder => self.button(
                "settings-open-logs",
                self.locations.log_dir.is_none(),
                Button::new("settings-open-logs", "open folder")
                    .icon(IconName::FolderOpen)
                    .disabled(self.locations.log_dir.is_none()),
                Rc::new(|this, _, cx| {
                    if let Some(dir) = this.locations.log_dir.clone() {
                        files::open(&dir, cx);
                    }
                }),
                Region::Page,
                theme,
                cx,
            ),
            Setting::ConfigFolder => self.button(
                "settings-open-config",
                self.locations.config_dir.is_none(),
                Button::new("settings-open-config", "open folder")
                    .icon(IconName::FolderOpen)
                    .disabled(self.locations.config_dir.is_none()),
                Rc::new(|this, _, cx| {
                    if let Some(dir) = this.locations.config_dir.clone() {
                        // A first start may not have written anything yet.
                        let _ = std::fs::create_dir_all(&dir);
                        files::open(&dir, cx);
                    }
                }),
                Region::Page,
                theme,
                cx,
            ),
            Setting::About => self.button(
                "settings-about",
                false,
                Button::new("settings-about", "about"),
                Rc::new(|_, _, cx| cx.emit(SettingsEvent::About)),
                Region::Page,
                theme,
                cx,
            ),
        }
    }

    /// A switch Tab reaches; `apply` takes the new value.
    fn switch(
        &self,
        id: impl Into<SharedString>,
        on: bool,
        disabled: bool,
        apply: impl Fn(&mut Self, bool, &mut gpui::Window, &mut Context<Self>) + 'static,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = id.into();
        let apply = Rc::new(apply);
        let click = apply.clone();
        let control = Switch::new(id.clone(), on)
            .disabled(disabled)
            .on_change(cx.listener(move |this, value: &bool, window, cx| {
                click(this, *value, window, cx);
            }));
        if disabled {
            return control.into_any_element();
        }
        self.focusable(
            id,
            control,
            Rc::new(move |this, window, cx| apply(this, !on, window, cx)),
            None,
            theme,
            cx,
        )
    }

    /// A segmented control sized to its labels that Tab reaches, calling
    /// `handler` with the chosen index: ← → choose the neighbour, Space
    /// the next one round.
    fn segmented(
        &self,
        id: &'static str,
        options: &[&'static str],
        selected: usize,
        theme: &Theme,
        cx: &Context<Self>,
        handler: impl Fn(&mut SettingsPanel, usize, &mut gpui::Window, &mut Context<SettingsPanel>)
        + 'static,
    ) -> AnyElement {
        let count = options.len();
        let handler = Rc::new(handler);
        let click = handler.clone();
        let next = handler.clone();
        let control = options
            .iter()
            .fold(Segmented::new(id).hug(), |control, option| {
                control.option(*option)
            })
            .selected(selected)
            .on_select(cx.listener(move |this, index: &usize, window, cx| {
                click(this, *index, window, cx);
                cx.notify();
            }));
        self.focusable(
            id,
            control,
            Rc::new(move |this, window, cx| {
                next(this, (selected + 1) % count, window, cx);
                cx.notify();
            }),
            Some(Rc::new(move |this, forward, window, cx| {
                let index = if forward {
                    (selected + 1).min(count - 1)
                } else {
                    selected.saturating_sub(1)
                };
                if index != selected {
                    handler(this, index, window, cx);
                    cx.notify();
                }
            })),
            theme,
            cx,
        )
    }

    /// A button (`id`, built) Tab reaches unless `disabled`: Space or
    /// Enter does what a click does.
    #[expect(
        clippy::too_many_arguments,
        reason = "a button and where Tab finds it; a struct would only rename them"
    )]
    pub(super) fn button(
        &self,
        id: impl Into<SharedString>,
        disabled: bool,
        button: Button,
        action: Activate,
        region: Region,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let click = action.clone();
        let button = button.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            click(this, window, cx);
        }));
        if disabled {
            return button.into_any_element();
        }
        self.focusable_in(id, button, action, None, region, theme, cx)
    }

    /// A text field `width` wide (the frame's content width; the field
    /// adds its padding and border), red-framed while its value has a
    /// problem.
    fn field(&self, id: &FieldId, width: f32, theme: &Theme, cx: &App) -> AnyElement {
        let Some(input) = self.inputs.get(id) else {
            return empty();
        };
        let key = SharedString::from(format!("field-{id:?}"));
        self.add_stop(key.clone(), input.focus_handle(cx), Region::Page);
        div()
            .relative()
            .flex_none()
            .w(px(width + FIELD_FRAME))
            .child(
                TextField::new(input)
                    .bordered(true)
                    .invalid(self.errors.contains_key(id))
                    .text_size(theme.text.body),
            )
            .child(self.bounds_probe(key))
            .into_any_element()
    }

    /// Pausing every environment (acts at once), or the running pause and
    /// *resume*.
    fn pause_control(&self, theme: &Theme, facts: &Facts, cx: &Context<Self>) -> AnyElement {
        let count = facts.environments.len();
        match facts.paused_until {
            Some(until) => {
                let resume: Activate = Rc::new(|this, _, cx| {
                    this.state.update(cx, |state, cx| {
                        state.pause_notifications(None);
                        cx.notify();
                    });
                });
                let click = resume.clone();
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(theme.text.small)
                            .text_color(theme.states.text.warning)
                            .child(paused_text(count, until, facts.now)),
                    )
                    .child(self.focusable(
                        "settings-resume",
                        Chip::new("settings-resume", "resume").on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| click(this, window, cx),
                        )),
                        resume,
                        None,
                        theme,
                        cx,
                    ))
                    .into_any_element()
            }
            None => div()
                .flex()
                .gap(px(6.))
                .children(PauseChoice::ALL.map(|choice| {
                    let id = SharedString::from(format!("settings-pause-{choice:?}"));
                    let pause: Activate = Rc::new(move |this, _, cx| {
                        this.state.update(cx, |state, cx| {
                            state.pause_notifications(Some(choice.until(Timestamp::now())));
                            cx.notify();
                        });
                    });
                    let click = pause.clone();
                    self.focusable(
                        id.clone(),
                        Chip::new(id, choice.short_label()).on_click(cx.listener(
                            move |this, _: &ClickEvent, window, cx| click(this, window, cx),
                        )),
                        pause,
                        None,
                        theme,
                        cx,
                    )
                }))
                .into_any_element(),
        }
    }

    /// Chips that turn `flags` of `key`'s rule on and off.
    fn flag_chips(
        &self,
        key: &ScopeKey,
        flags: &[RuleFlag],
        rule: &Rule,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let prefix = scope_id(key);
        div()
            .flex()
            // The frames' 6px less a fraction: the chips round to whole
            // pixels here, and the states row's description (`Recoveries
            // follow problems that notified.`) fits whole beside them as
            // drawn.
            .gap(px(5.))
            .children(flags.iter().map(|flag| {
                let flag = *flag;
                let key = key.clone();
                let on = flag.get(rule);
                let id = SharedString::from(format!("{prefix}-{}", flag.label()));
                let toggle: Activate =
                    Rc::new(move |this, _, cx| this.set_flag(&key, flag, !on, cx));
                let click = toggle.clone();
                self.focusable(
                    id.clone(),
                    Chip::new(id, flag.label())
                        .selected(on)
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            click(this, window, cx);
                        })),
                    toggle,
                    None,
                    theme,
                    cx,
                )
            }))
            .into_any_element()
    }

    /// A switch for `flag` of `key`'s rule.
    fn flag_switch(
        &self,
        key: &ScopeKey,
        flag: RuleFlag,
        rule: &Rule,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let id = SharedString::from(format!("{}-{flag:?}", scope_id(key)));
        let key = key.clone();
        self.switch(
            id,
            flag.get(rule),
            false,
            move |this, on, _, cx| this.set_flag(&key, flag, on, cx),
            theme,
            cx,
        )
    }

    /// A select showing `value` (after `dot`), its list (`menu`) opening
    /// from it; Tab reaches it, Space or Enter opens it, ← → `step` to the
    /// neighbouring choice.
    #[expect(
        clippy::too_many_arguments,
        reason = "a trigger and its menu; splitting it would only scatter them"
    )]
    fn dropdown(
        &self,
        id: &'static str,
        which: SettingsMenu,
        value: String,
        dot: Option<gpui::Hsla>,
        width: f32,
        menu: Menu,
        step: Step,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.menus.is_open(&which);
        let trigger = div().flex_none().w(px(width + FIELD_FRAME)).child(
            Select::new(id, value)
                .when_some(dot, Select::dot)
                .open(open)
                .when(open, |select| select.menu(menu))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.menus.toggle(which, down_position(event));
                    cx.notify();
                })),
        );
        self.focusable(
            id,
            trigger,
            Rc::new(move |this, _, cx| {
                this.menus.toggle(which, None);
                cx.notify();
            }),
            Some(step),
            theme,
            cx,
        )
    }

    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&ic_ui_kit::Dismissal, &mut gpui::Window, &mut App) + 'static {
        cx.listener(|this, dismissal: &ic_ui_kit::Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }

    /// The environments, each with its health dot; the page's one has the
    /// selected-row background.
    fn environment_menu(&self, theme: &Theme, facts: &Facts, cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("settings-environment-menu");
        for environment in &facts.environments {
            let id = environment.id.clone();
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("settings-environment-{}", environment.id).into()),
                    environment.name.clone(),
                )
                .dot(crate::sidebar::health_color(environment.health, theme))
                .checked(self.environment.as_deref() == Some(environment.id.as_str()))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.menus.close();
                    this.choose_environment(&id, window, cx);
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    /// The log levels, quietest first; the one in effect has the
    /// selected-row background.
    fn log_level_menu(facts: &Facts, cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("settings-log-level-menu");
        for level in LogLevel::ALL {
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("settings-log-level-{}", level.as_str()).into()),
                    level.as_str(),
                )
                .checked(facts.general.log_level == level)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.change_general(cx, |general| general.log_level = level);
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    // --- Rows of data ---------------------------------------------------

    /// The data rows of `section` that match `query` (normalized; empty:
    /// all of them). A section found by its name brings all of its rows.
    fn data_items(&self, section: Section, facts: &Facts, query: &str) -> Vec<DataItem> {
        let all = query.is_empty() || section_matches(section, query);
        let hit = |text: &str| all || contains(text, query);
        match section {
            Section::WatchedAndMuted => facts
                .overrides
                .iter()
                .enumerate()
                .filter(|(_, entry)| hit(&entry.label) || hit(&entry.text))
                .map(|(index, _)| DataItem::Override(index))
                .collect(),
            Section::GroupsAndDashboards => {
                let Some(plan) = &facts.plan else {
                    return Vec::new();
                };
                let mut items = Vec::new();
                for (group_index, group) in plan.groups.iter().enumerate() {
                    if hit(&group.name) {
                        items.push(DataItem::Group(group_index));
                    }
                    for (index, (_, name, _)) in group.dashboards.iter().enumerate() {
                        if hit(name) {
                            items.push(DataItem::Dashboard(group_index, index));
                        }
                    }
                }
                items
            }
            Section::Environments => facts
                .environments
                .iter()
                .enumerate()
                // By name and hosts, never by what the connection does
                // now (that changes while one looks).
                .filter(|(_, environment)| hit(&environment.name) || hit(&environment.hosts))
                .map(|(index, _)| DataItem::Environment(index))
                .collect(),
            Section::Shortcuts => {
                let filter = if query.is_empty() {
                    self.keymap_query.clone()
                } else if all {
                    String::new()
                } else {
                    query.to_owned()
                };
                facts
                    .shortcuts
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.matches(&filter))
                    .map(|(index, _)| DataItem::Shortcut(index))
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The keymap page's rows as listed (the filter applied), for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn shortcut_labels(&self, cx: &App) -> Vec<String> {
        let facts = self.facts(cx);
        self.data_items(Section::Shortcuts, &facts, "")
            .iter()
            .filter_map(|item| match item {
                DataItem::Shortcut(index) => facts.shortcuts.get(*index),
                _ => None,
            })
            .map(|row| row.label.clone())
            .collect()
    }

    /// The keymap page's filter field, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn keymap_filter(&self) -> &gpui::Entity<ic_ui_kit::input::InputState> {
        &self.keymap_filter
    }

    /// How many data rows of `page` match the search.
    pub(super) fn data_matches(&self, page: SettingsPage, facts: &Facts, _cx: &App) -> usize {
        page.sections()
            .iter()
            .map(|section| self.data_items(*section, facts, &self.query).len())
            .sum()
    }

    /// The rows of a data item; `full` adds a custom rule's rows under a
    /// group or dashboard (the page, not the search).
    fn render_item(
        &self,
        item: &DataItem,
        full: bool,
        theme: &Theme,
        facts: &Facts,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        match item {
            DataItem::Override(index) => facts
                .overrides
                .get(*index)
                .map(|entry| vec![self.override_row(*index, entry, theme, cx)])
                .unwrap_or_default(),
            DataItem::Group(group_index) => {
                let Some(group) = facts
                    .plan
                    .as_ref()
                    .and_then(|plan| plan.groups.get(*group_index))
                else {
                    return Vec::new();
                };
                let key = ScopeKey::Group(group.id.clone());
                let parent = facts.environment_name.clone().unwrap_or_default();
                let mark = div()
                    .flex()
                    .flex_none()
                    .w(px(14.))
                    .justify_center()
                    .child(
                        Icon::new(IconName::Folder)
                            .size(px(13.))
                            .color(theme.colors.text_muted),
                    )
                    .into_any_element();
                let mut rows = vec![self.scope_row(
                    &key,
                    mark,
                    &group.name,
                    &group.setting,
                    &parent,
                    0.,
                    theme,
                    cx,
                )];
                if full && let ScopeSetting::Custom(rule) = &group.setting {
                    rows.extend(self.rule_rows(&key, rule, SUB_INDENT, theme, cx));
                }
                rows
            }
            DataItem::Dashboard(group_index, index) => {
                let Some(group) = facts
                    .plan
                    .as_ref()
                    .and_then(|plan| plan.groups.get(*group_index))
                else {
                    return Vec::new();
                };
                let Some((id, name, setting)) = group.dashboards.get(*index) else {
                    return Vec::new();
                };
                let key = ScopeKey::Dashboard(group.id.clone(), id.clone());
                let summary = facts
                    .dashboard_summaries
                    .get(&(group.id.clone(), id.clone()));
                let mut rows = vec![self.scope_row(
                    &key,
                    mark_dot(summary_color(summary, theme)),
                    name,
                    setting,
                    &group.name,
                    SUB_INDENT,
                    theme,
                    cx,
                )];
                if full && let ScopeSetting::Custom(rule) = setting {
                    rows.extend(self.rule_rows(&key, rule, 2. * SUB_INDENT, theme, cx));
                }
                rows
            }
            DataItem::Environment(index) => facts
                .environments
                .get(*index)
                .map(|environment| vec![self.environment_row(environment, theme, cx)])
                .unwrap_or_default(),
            DataItem::Shortcut(index) => facts
                .shortcuts
                .get(*index)
                .map(|row| self.keymap_rows(&[row], theme))
                .unwrap_or_default(),
        }
    }

    /// A watched or muted object: its dot, `service on host`, what is
    /// set, and *remove*.
    fn override_row(
        &self,
        index: usize,
        entry: &OverrideFact,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let object = entry.object.clone();
        let dot = entry.state.map_or(theme.states.fill.pending, |state| {
            theme.states.fill.checkable(state)
        });
        let label = match &entry.object {
            ObjectKey::Service { key } => div()
                .flex()
                .min_w_0()
                .child(div().text_color(colors.text_strong).child(rows::marked(
                    &key.name,
                    &self.query,
                    theme,
                )))
                .child(div().text_color(colors.text_faint).child("\u{a0}on\u{a0}"))
                .child(div().text_color(colors.text_secondary).child(rows::marked(
                    key.host.as_str(),
                    &self.query,
                    theme,
                ))),
            ObjectKey::Host { name } => div()
                .flex()
                .min_w_0()
                .text_color(colors.text_strong)
                .child(rows::marked(name.as_str(), &self.query, theme)),
        };
        Row::new(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .min_w_0()
                .child(mark_dot(dot))
                .child(label)
                .child(
                    div()
                        .flex_none()
                        .text_size(theme.text.small)
                        .text_color(if entry.watched {
                            colors.accent_text
                        } else {
                            colors.text_muted
                        })
                        .child(entry.text.clone()),
                ),
        )
        .control({
            let id = SharedString::from(format!("settings-override-remove-{index}"));
            self.button(
                id.clone(),
                false,
                Button::new(id, "remove")
                    .tooltip(Tooltip::new("Notifications follow the dashboards again")),
                Rc::new(move |this, _, cx| {
                    let object = object.clone();
                    this.change_plan(cx, move |plan| {
                        plan.settings.objects.retain(|entry| entry.object != object);
                    });
                }),
                Region::Page,
                theme,
                cx,
            )
        })
        .render(theme)
    }

    /// A group's or dashboard's notification setting: inherit, on, off,
    /// or a custom rule of its own.
    #[expect(
        clippy::too_many_arguments,
        reason = "a row's parts; a struct would only rename them"
    )]
    fn scope_row(
        &self,
        key: &ScopeKey,
        mark: AnyElement,
        name: &str,
        setting: &ScopeSetting,
        parent: &str,
        indent: f32,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let choose = key.clone();
        let id = SharedString::from(format!("{}-setting", scope_id(key)));
        let selected = scope_choice(setting);
        let count = SCOPE_CHOICES.len();
        let choice: Choose =
            Rc::new(move |this, index, window, cx| this.choose_scope(&choose, index, window, cx));
        let click = choice.clone();
        let next = choice.clone();
        let control = self.focusable(
            id.clone(),
            SCOPE_CHOICES
                .iter()
                .fold(Segmented::new(id).hug(), |control, choice| {
                    control.option(*choice)
                })
                .selected(selected)
                .on_select(cx.listener(move |this, index: &usize, window, cx| {
                    click(this, *index, window, cx);
                })),
            Rc::new(move |this, window, cx| next(this, (selected + 1) % count, window, cx)),
            Some(Rc::new(move |this, forward, window, cx| {
                let index = if forward {
                    (selected + 1).min(count - 1)
                } else {
                    selected.saturating_sub(1)
                };
                if index != selected {
                    choice(this, index, window, cx);
                }
            })),
            theme,
            cx,
        );
        Row::new(
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .min_w_0()
                .child(mark)
                .child(rows::marked(name, &self.query, theme)),
        )
        .description(Some(
            div().pl(px(24.)).child(scope_meaning(setting, parent)),
        ))
        .indent(indent)
        .control(control)
        .render(theme)
    }

    /// The rows of a group's or dashboard's custom rule, `indent` deep.
    fn rule_rows(
        &self,
        key: &ScopeKey,
        rule: &Rule,
        indent: f32,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let field = FieldId::MinDuration(key.clone());
        let mut rows = vec![
            Row::new("states")
                .description(Some("Recoveries follow problems that notified."))
                .control(self.flag_chips(key, &RuleFlag::STATES, rule, theme, cx)),
            Row::new("events")
                .description(Some("Also notify when these start or end."))
                .control(self.flag_chips(key, &RuleFlag::EVENTS, rule, theme, cx)),
        ];
        for flag in [RuleFlag::HardOnly, RuleFlag::SkipHandled] {
            rows.push(Row::new(flag.label()).control(self.flag_switch(key, flag, rule, theme, cx)));
        }
        rows.push(
            Row::new("only after")
                .description(Some(
                    "A problem must last this long first (5m, 1h; 0 = at once).",
                ))
                .error(self.errors.get(&field).cloned())
                .control(self.field(&field, MIN_DURATION_FIELD, theme, cx)),
        );
        rows.push(Row::new(RuleFlag::Sound.label()).control(self.flag_switch(
            key,
            RuleFlag::Sound,
            rule,
            theme,
            cx,
        )));
        rows.into_iter()
            .map(|row| row.indent(indent).render(theme))
            .collect()
    }

    /// An environment: its health dot, name and connection in a line, and
    /// a gear that opens its editor.
    fn environment_row(
        &self,
        environment: &EnvironmentFact,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let id = environment.id.clone();
        div()
            .id(SharedString::from(format!(
                "settings-environment-row-{}",
                environment.id
            )))
            .flex()
            .items_center()
            .gap(px(10.))
            .py(px(12.))
            .min_h(px(58.))
            .border_t_1()
            .border_color(colors.border_row)
            .child(mark_dot(crate::sidebar::health_color(
                environment.health,
                theme,
            )))
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.row)
                    .text_color(colors.text_strong)
                    .child(rows::marked(&environment.name, &self.query, theme)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(rows::marked(&environment.summary, &self.query, theme)),
            )
            .child({
                let gear =
                    SharedString::from(format!("settings-edit-environment-{}", environment.id));
                let edit: Activate = Rc::new(move |_, _, cx| {
                    cx.emit(SettingsEvent::EditEnvironment(id.clone()));
                });
                let click = edit.clone();
                self.focusable(
                    gear.clone(),
                    IconButton::new(gear, IconName::Settings)
                        .icon_size(px(13.))
                        .color(colors.text_muted)
                        .tooltip(Tooltip::new(format!("Edit {}", environment.name)))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            click(this, window, cx);
                        })),
                    edit,
                    None,
                    theme,
                    cx,
                )
            })
            .into_any_element()
    }

    // --- Keymap -----------------------------------------------------------

    /// The keymap page: the filter, what the keymap file couldn't bind,
    /// and the table.
    fn render_keymap(&self, theme: &Theme, facts: &Facts, cx: &Context<Self>) -> Vec<AnyElement> {
        let colors = theme.colors;
        let mut blocks = vec![
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .mt(px(16.))
                .mb(px(10.))
                .h(theme.metrics.field_height)
                .px(px(10.))
                .rounded(theme.metrics.code_radius)
                .border_1()
                .border_color(colors.border_header)
                .bg(colors.code_background)
                .child(
                    Icon::new(IconName::Search)
                        .size(px(13.))
                        .color(colors.text_muted),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(TextField::new(&self.keymap_filter).text_size(theme.text.body)),
                )
                .into_any_element(),
        ];
        self.add_stop(
            "settings-keymap-filter".into(),
            self.keymap_filter.focus_handle(cx),
            Region::Page,
        );
        if !facts.keymap_problems.is_empty() {
            let count = facts.keymap_problems.len();
            let noun = if count == 1 { "binding" } else { "bindings" };
            blocks.push(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .pb(px(10.))
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.critical)
                    .child(format!(
                        "keymap.toml: {count} {noun} skipped (the log has every one)"
                    ))
                    .children(
                        facts
                            .keymap_problems
                            .iter()
                            .take(3)
                            .map(|problem| div().truncate().child(problem.clone())),
                    )
                    .into_any_element(),
            );
        }
        blocks.push(
            div()
                .flex()
                .items_center()
                .gap(px(COLUMN_GAP))
                .py(px(6.))
                .text_size(theme.text.label)
                .text_color(colors.text_faint)
                .child(div().flex_1().min_w_0().child("action"))
                .child(div().flex_none().w(px(KEYS_COLUMN)).child("keys"))
                .child(div().flex_none().w(px(PLACE_COLUMN)).child("where"))
                .into_any_element(),
        );
        let shortcuts: Vec<&ShortcutRow> = self
            .data_items(Section::Shortcuts, facts, "")
            .iter()
            .filter_map(|item| match item {
                DataItem::Shortcut(index) => facts.shortcuts.get(*index),
                _ => None,
            })
            .collect();
        if shortcuts.is_empty() {
            blocks.push(
                div()
                    .py(px(12.))
                    .border_t_1()
                    .border_color(colors.border_row)
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child("No shortcut matches.")
                    .into_any_element(),
            );
        }
        blocks.extend(self.keymap_rows(&shortcuts, theme));
        let _ = cx;
        blocks
    }

    /// Rows of the keymap table.
    fn keymap_rows(&self, shortcuts: &[&ShortcutRow], theme: &Theme) -> Vec<AnyElement> {
        let colors = theme.colors;
        shortcuts
            .iter()
            .map(|row| {
                let keys: Vec<AnyElement> = if row.range && row.keys.len() == 2 {
                    vec![
                        rows::key_cap(&row.keys[0], theme),
                        div()
                            .text_size(theme.text.small)
                            .text_color(colors.text_faint)
                            .child("to")
                            .into_any_element(),
                        rows::key_cap(&row.keys[1], theme),
                    ]
                } else {
                    row.keys
                        .iter()
                        .map(|key| rows::key_cap(key, theme))
                        .collect()
                };
                div()
                    .flex()
                    .items_center()
                    .gap(px(COLUMN_GAP))
                    .py(px(9.))
                    .border_t_1()
                    .border_color(colors.border_row)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text.body)
                            .text_color(colors.text_strong)
                            .child(rows::marked(&row.label, &self.query, theme)),
                    )
                    .child(
                        // Many keys wrap to a second line rather than run
                        // into the next column.
                        div()
                            .flex()
                            .flex_wrap()
                            .flex_none()
                            .items_center()
                            .gap(px(4.))
                            .w(px(KEYS_COLUMN))
                            .children(keys),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(PLACE_COLUMN))
                            .truncate()
                            .text_size(theme.text.small)
                            .text_color(colors.text_faint)
                            .child(row.place.clone()),
                    )
                    .into_any_element()
            })
            .collect()
    }

    // --- Appearance preview -------------------------------------------

    /// The preview: the selected dashboard's first rows (sample rows while
    /// nothing is loaded) as the dashboard list draws them, in the row
    /// density and with the times the settings ask for (the density is the
    /// theme's, so it follows at once).
    fn render_preview(theme: &Theme, facts: &Facts) -> AnyElement {
        let colors = theme.colors;
        let rows = if facts.preview.1.is_empty() {
            sample_rows()
        } else {
            facts.preview.1.clone()
        };
        let clock = facts.appearance.list_times == ListTimes::Clock;
        div()
            .flex()
            .flex_col()
            .mt(px(2.))
            .rounded(px(6.))
            .border_1()
            .border_color(colors.border_header)
            .overflow_hidden()
            .children(rows.into_iter().enumerate().map(|(index, row)| {
                let time = if clock { row.clock } else { row.relative };
                let mut line = ListRow::new(("settings-preview", index))
                    .state(StateCircle::new(row.state).handled(row.handled), time)
                    .title(row.name)
                    .detail(row.output);
                if let Some(host) = row.host {
                    line = line.context("on", host);
                }
                line.into_any_element()
            }))
            .into_any_element()
    }
}

/// Rows for the preview while no dashboard has any: what the demo's
/// databases dashboard shows.
fn sample_rows() -> Vec<PreviewRow> {
    use ic_model::ServiceState;
    let row =
        |state, name: &str, host: &str, output: &str, relative: &str, clock: &str| PreviewRow {
            state: CheckableState::Service(state),
            handled: false,
            name: name.to_owned(),
            host: Some(host.to_owned()),
            output: output.to_owned(),
            relative: relative.to_owned(),
            clock: clock.to_owned(),
        };
    vec![
        row(
            ServiceState::Critical,
            "postgres-replication",
            "db-prod-03",
            "CRITICAL - standby lag 412s (> 300s)",
            "14m",
            "13:58",
        ),
        row(
            ServiceState::Warning,
            "pg-connections",
            "db-prod-03",
            "WARNING - 182 of 200 per-db limit (orders)",
            "22m",
            "13:50",
        ),
        row(
            ServiceState::Unknown,
            "mysql-replication",
            "db-mysql-03",
            "UNKNOWN - connection refused",
            "2h",
            "12:09",
        ),
    ]
}

/// The choice before (`forward` false) or after `current` in `items`,
/// if there is one.
fn neighbour<T: Clone + PartialEq>(items: &[T], current: Option<&T>, forward: bool) -> Option<T> {
    let index = items.iter().position(|item| Some(item) == current)?;
    let next = if forward {
        index.checked_add(1)?
    } else {
        index.checked_sub(1)?
    };
    items.get(next).cloned()
}

/// Nothing (a control that can't be shown without an environment).
fn empty() -> AnyElement {
    div().into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_name_the_hosts_the_urls_and_the_connection() {
        let mut environment = ic_config::Environment::new(
            "prod-cluster",
            "https://master-01:5665",
            ic_config::AuthConfig::Basic {
                username: "icygui".to_owned(),
            },
        );
        environment
            .urls
            .push(ic_config::ApiUrl::new("https://master-02.example.com:5665"));
        assert_eq!(
            environment_summary(&environment, None, Timestamp::now()),
            "master-01, master-02.example.com · 2 URLs · starting"
        );
        // Two ports of one host name it once.
        environment.urls[1] = ic_config::ApiUrl::new("https://master-01:5666");
        assert_eq!(url_hosts(&environment), ["master-01"]);
        let mut connection = ConnectionStatus::starting("master-01", None);
        assert_eq!(
            connection_words(&connection, &["master-01".to_owned()], Timestamp::now()),
            "connecting"
        );
        connection.state = None;
        assert_eq!(
            connection_words(&connection, &[], Timestamp::now()),
            "not started"
        );
    }

    #[test]
    fn dashboard_dots_follow_their_counts() {
        let theme = Theme::dark();
        assert_eq!(summary_color(None, &theme), theme.states.fill.pending);
        let mut summary = Summary::default();
        assert_eq!(
            summary_color(Some(&summary), &theme),
            theme.states.fill.pending
        );
        summary.ok = 3;
        assert_eq!(summary_color(Some(&summary), &theme), theme.states.fill.ok);
        summary.worst_unhandled = Some(CheckableState::Service(ic_model::ServiceState::Critical));
        assert_eq!(
            summary_color(Some(&summary), &theme),
            theme.states.fill.critical
        );
    }

    #[test]
    fn sample_rows_fill_the_preview() {
        assert_eq!(sample_rows().len(), PREVIEW_ROWS);
    }
}
