//! The tray menu: a platform-independent model of its entries, the ids its
//! items carry and how they map back to [`TrayCommand`]s, and the muda menu
//! built from the model.
//!
//! The menu is rebuilt from the model when the model changes (pause state,
//! environments). That is rare, and rebuilding keeps the code free of
//! in-place bookkeeping; on macOS it closes the menu if it happens to be
//! open, on Linux the host just reloads the layout.

use std::time::Duration;

use jiff::Zoned;
use tray_icon::menu::{
    CheckMenuItem, Error as MenuError, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu,
};

use super::TrayCommand;

/// Prefix of every id the tray's items carry.
const PREFIX: &str = "icygui.tray.";
const OPEN_ID: &str = "icygui.tray.open";
const RESUME_ID: &str = "icygui.tray.resume";
const QUIT_ID: &str = "icygui.tray.quit";
const PAUSE_PREFIX: &str = "icygui.tray.pause.";
const ENVIRONMENT_PREFIX: &str = "icygui.tray.environment.";
/// Ids of entries that do nothing when clicked.
const STATUS_ID: &str = "icygui.tray.status";
const PAUSE_MENU_ID: &str = "icygui.tray.pause";
const ENVIRONMENT_MENU_ID: &str = "icygui.tray.environments";

/// Longest label, in characters; longer names are shortened with "…".
const MAX_LABEL_CHARS: usize = 60;

/// Local hour at which "until tomorrow morning" ends.
const MORNING_HOUR: i8 = 8;
/// Until this local hour the night still counts as the previous day, so a
/// pause started at 02:00 ends the same morning.
const NIGHT_ENDS_HOUR: i8 = 4;
/// Used if the end of the morning pause can't be computed (a date beyond
/// what the calendar library supports).
const FALLBACK_PAUSE: Duration = Duration::from_hours(8);

/// A choice under "Pause notifications" (PLAN.md §2.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum PausePreset {
    /// 30 minutes.
    HalfHour,
    /// 1 hour.
    Hour,
    /// Until 08:00 local time on the next morning.
    UntilMorning,
}

impl PausePreset {
    pub(crate) const ALL: [Self; 3] = [Self::HalfHour, Self::Hour, Self::UntilMorning];

    fn id_suffix(self) -> &'static str {
        match self {
            Self::HalfHour => "30m",
            Self::Hour => "1h",
            Self::UntilMorning => "tomorrow",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::HalfHour => "For 30 minutes",
            Self::Hour => "For 1 hour",
            Self::UntilMorning => "Until tomorrow morning",
        }
    }

    /// How long to pause when chosen now. `now` is only called for the
    /// preset that depends on the time of day.
    pub(crate) fn duration(self, now: impl FnOnce() -> Zoned) -> Duration {
        match self {
            Self::HalfHour => Duration::from_mins(30),
            Self::Hour => Duration::from_hours(1),
            Self::UntilMorning => until_morning(&now()),
        }
    }
}

/// The time until the next morning's [`MORNING_HOUR`], in the time zone of
/// `now`, so daylight-saving changes in between are accounted for.
///
/// "Tomorrow" is the next calendar day, except that the hours before
/// [`NIGHT_ENDS_HOUR`] still belong to the previous day: pausing at 02:00
/// ends at 08:00 the same morning, not 30 hours later. For an on-call
/// engineer a pause that is too long is worse than one that is too short.
fn until_morning(now: &Zoned) -> Duration {
    let duration = morning_after(now).map(|end| Duration::try_from(now.duration_until(&end)));
    match duration {
        Ok(Ok(duration)) if !duration.is_zero() => duration,
        _ => {
            tracing::warn!(%now, "cannot compute the next morning; pausing for 8 hours");
            FALLBACK_PAUSE
        }
    }
}

fn morning_after(now: &Zoned) -> Result<Zoned, jiff::Error> {
    let day = if now.hour() < NIGHT_ENDS_HOUR {
        now.date()
    } else {
        now.date().tomorrow()?
    };
    day.at(MORNING_HOUR, 0, 0, 0)
        .to_zoned(now.time_zone().clone())
}

/// What a menu item does when clicked.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum MenuAction {
    Open,
    Pause(PausePreset),
    Resume,
    SwitchEnvironment(String),
    Quit,
}

impl MenuAction {
    /// The id the item carries; [`MenuAction::from_id`] reverses it.
    pub(crate) fn id(&self) -> String {
        match self {
            Self::Open => OPEN_ID.to_owned(),
            Self::Pause(preset) => format!("{PAUSE_PREFIX}{}", preset.id_suffix()),
            Self::Resume => RESUME_ID.to_owned(),
            Self::SwitchEnvironment(id) => format!("{ENVIRONMENT_PREFIX}{id}"),
            Self::Quit => QUIT_ID.to_owned(),
        }
    }

    /// The action of a clicked item; `None` for ids that aren't the
    /// tray's or belong to entries without an action (status lines,
    /// submenu titles).
    pub(crate) fn from_id(id: &str) -> Option<Self> {
        let rest = id.strip_prefix(PREFIX)?;
        match rest {
            "open" => return Some(Self::Open),
            "resume" => return Some(Self::Resume),
            "quit" => return Some(Self::Quit),
            _ => {}
        }
        if let Some(environment) = id.strip_prefix(ENVIRONMENT_PREFIX) {
            return Some(Self::SwitchEnvironment(environment.to_owned()));
        }
        let suffix = id.strip_prefix(PAUSE_PREFIX)?;
        PausePreset::ALL
            .into_iter()
            .find(|preset| preset.id_suffix() == suffix)
            .map(Self::Pause)
    }

    /// The command for the app. `now` is only called for pauses that
    /// depend on the time of day.
    pub(crate) fn command(self, now: impl FnOnce() -> Zoned) -> TrayCommand {
        match self {
            Self::Open => TrayCommand::Open,
            Self::Pause(preset) => TrayCommand::PauseFor(preset.duration(now)),
            Self::Resume => TrayCommand::Resume,
            Self::SwitchEnvironment(id) => TrayCommand::SwitchEnvironment(id),
            Self::Quit => TrayCommand::Quit,
        }
    }
}

/// What the menu shows; the [`Tray`](super::Tray) setters change it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MenuState {
    pub(crate) app_name: String,
    /// `(id, name)` in display order.
    pub(crate) environments: Vec<(String, String)>,
    /// The active environment's id.
    pub(crate) active: Option<String>,
    /// When notifications resume, as the app formats it; `None` when not
    /// paused.
    pub(crate) paused_until: Option<String>,
}

impl MenuState {
    pub(crate) fn new(app_name: &str) -> Self {
        Self {
            app_name: app_name.to_owned(),
            environments: Vec::new(),
            active: None,
            paused_until: None,
        }
    }
}

/// One entry of the menu, with display text (not yet escaped for muda).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// A clickable item.
    Item {
        action: MenuAction,
        label: String,
    },
    /// One of a set of choices. The current one is checked and disabled,
    /// so clicking it can't untick it; the others are unchecked.
    Choice {
        action: MenuAction,
        label: String,
        current: bool,
    },
    /// A disabled line of information.
    Status(String),
    /// A submenu.
    Submenu {
        id: &'static str,
        label: String,
        entries: Vec<Entry>,
    },
    Separator,
}

/// The menu for `state`:
///
/// ```text
/// Open icygui
/// ─────────────
/// Paused until 18:30          (while paused)
/// Resume notifications        (while paused)
/// Pause notifications       ▸ For 30 minutes / For 1 hour / Until tomorrow morning
/// ─────────────               (with environments)
/// Environment               ▸ ✓ prod-cluster / staging / …
/// ─────────────
/// Quit icygui
/// ```
pub(crate) fn entries(state: &MenuState) -> Vec<Entry> {
    let app = clean_label(&state.app_name);
    let mut entries = vec![
        Entry::Item {
            action: MenuAction::Open,
            label: format!("Open {app}"),
        },
        Entry::Separator,
    ];
    if let Some(until) = &state.paused_until {
        entries.push(Entry::Status(clean_label(&format!("Paused until {until}"))));
        entries.push(Entry::Item {
            action: MenuAction::Resume,
            label: "Resume notifications".to_owned(),
        });
    }
    entries.push(Entry::Submenu {
        id: PAUSE_MENU_ID,
        label: "Pause notifications".to_owned(),
        entries: PausePreset::ALL
            .into_iter()
            .map(|preset| Entry::Item {
                action: MenuAction::Pause(preset),
                label: preset.label().to_owned(),
            })
            .collect(),
    });
    if !state.environments.is_empty() {
        entries.push(Entry::Separator);
        entries.push(Entry::Submenu {
            id: ENVIRONMENT_MENU_ID,
            label: "Environment".to_owned(),
            entries: state
                .environments
                .iter()
                .map(|(id, name)| Entry::Choice {
                    action: MenuAction::SwitchEnvironment(id.clone()),
                    label: environment_label(id, name),
                    current: state.active.as_deref() == Some(id.as_str()),
                })
                .collect(),
        });
    }
    entries.push(Entry::Separator);
    entries.push(Entry::Item {
        action: MenuAction::Quit,
        label: format!("Quit {app}"),
    });
    entries
}

/// The environment's name, or its id if the name is blank.
fn environment_label(id: &str, name: &str) -> String {
    let name = clean_label(name);
    if !name.is_empty() {
        return name;
    }
    let id = clean_label(id);
    if id.is_empty() {
        "(unnamed)".to_owned()
    } else {
        id
    }
}

/// One line of menu text: control characters (newlines, tabs, escape
/// sequences) become spaces, surrounding space is trimmed, and text longer
/// than [`MAX_LABEL_CHARS`] is shortened with "…".
pub(crate) fn clean_label(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = cleaned.trim();
    if trimmed.chars().count() <= MAX_LABEL_CHARS {
        return trimmed.to_owned();
    }
    let mut short: String = trimmed.chars().take(MAX_LABEL_CHARS - 1).collect();
    short.truncate(short.trim_end().len());
    short.push('…');
    short
}

/// Text for muda, which treats `&` as a mnemonic marker (`&&` is a
/// literal `&`).
fn muda_text(label: &str) -> String {
    label.replace('&', "&&")
}

/// Builds the muda menu for `entries`.
///
/// # Errors
///
/// muda refuses an item (only possible for cycles or foreign items, which
/// this never creates).
pub(crate) fn build(entries: &[Entry]) -> Result<Menu, MenuError> {
    let menu = Menu::new();
    for entry in entries {
        menu.append(item(entry)?.as_ref())?;
    }
    Ok(menu)
}

fn item(entry: &Entry) -> Result<Box<dyn IsMenuItem>, MenuError> {
    Ok(match entry {
        Entry::Item { action, label } => {
            Box::new(MenuItem::with_id(action.id(), muda_text(label), true, None))
        }
        Entry::Choice {
            action,
            label,
            current,
        } => Box::new(CheckMenuItem::with_id(
            action.id(),
            muda_text(label),
            !current,
            *current,
            None,
        )),
        Entry::Status(text) => Box::new(MenuItem::with_id(STATUS_ID, muda_text(text), false, None)),
        Entry::Submenu { id, label, entries } => {
            let submenu = Submenu::with_id(*id, muda_text(label), true);
            for entry in entries {
                submenu.append(item(entry)?.as_ref())?;
            }
            Box::new(submenu)
        }
        Entry::Separator => Box::new(PredefinedMenuItem::separator()),
    })
}

#[cfg(test)]
mod tests {
    use jiff::civil::date;
    use jiff::tz::TimeZone;

    use super::*;

    fn state() -> MenuState {
        MenuState::new("icygui")
    }

    fn fixed_actions() -> Vec<MenuAction> {
        let mut actions = vec![MenuAction::Open, MenuAction::Resume, MenuAction::Quit];
        actions.extend(PausePreset::ALL.map(MenuAction::Pause));
        actions
    }

    fn central_europe() -> TimeZone {
        TimeZone::posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap()
    }

    fn at(tz: &TimeZone, year: i16, month: i8, day: i8, hour: i8, minute: i8) -> Zoned {
        date(year, month, day)
            .at(hour, minute, 0, 0)
            .to_zoned(tz.clone())
            .unwrap()
    }

    fn hours(hours: f64) -> Duration {
        Duration::from_secs_f64(hours * 3600.0)
    }

    fn morning_pause(now: &Zoned) -> Duration {
        PausePreset::UntilMorning.duration(|| now.clone())
    }

    #[test]
    fn ids_round_trip() {
        for action in fixed_actions() {
            let id = action.id();
            assert!(id.starts_with(PREFIX), "{id}");
            assert_eq!(MenuAction::from_id(&id), Some(action), "{id}");
        }
        for environment in [
            "6f1c0d7e-2b4a-4c8e-9a51-0f3e7d2c1b9a",
            "",
            "with.dots",
            "icygui.tray.quit",
            "pause.30m",
            "ümlaut and spaces",
        ] {
            let action = MenuAction::SwitchEnvironment(environment.to_owned());
            assert_eq!(
                MenuAction::from_id(&action.id()),
                Some(action),
                "{environment:?}"
            );
        }
    }

    #[test]
    fn ids_are_distinct() {
        let mut ids: Vec<String> = fixed_actions().iter().map(MenuAction::id).collect();
        ids.push(MenuAction::SwitchEnvironment("x".to_owned()).id());
        let count = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count);
    }

    #[test]
    fn foreign_and_inert_ids_have_no_action() {
        for id in [
            "",
            "open",
            "quit",
            "icygui.tray.",
            "icygui.tray.Open",
            "icygui.tray.open ",
            "icygui.tray.pause",
            "icygui.tray.pause.",
            "icygui.tray.pause.2h",
            "icygui.tray.environments",
            "icygui.tray.environment",
            "icygui.tray.status",
            "other.app.quit",
            "1",
        ] {
            assert_eq!(MenuAction::from_id(id), None, "{id:?}");
        }
        assert_eq!(MenuAction::from_id(STATUS_ID), None);
        assert_eq!(MenuAction::from_id(PAUSE_MENU_ID), None);
        assert_eq!(MenuAction::from_id(ENVIRONMENT_MENU_ID), None);
    }

    #[test]
    fn actions_map_to_commands() {
        let never = || -> Zoned { panic!("the clock is not needed") };
        assert_eq!(MenuAction::Open.command(never), TrayCommand::Open);
        assert_eq!(MenuAction::Resume.command(never), TrayCommand::Resume);
        assert_eq!(MenuAction::Quit.command(never), TrayCommand::Quit);
        assert_eq!(
            MenuAction::SwitchEnvironment("staging-id".to_owned()).command(never),
            TrayCommand::SwitchEnvironment("staging-id".to_owned())
        );
        assert_eq!(
            MenuAction::Pause(PausePreset::HalfHour).command(never),
            TrayCommand::PauseFor(Duration::from_mins(30))
        );
        assert_eq!(
            MenuAction::Pause(PausePreset::Hour).command(never),
            TrayCommand::PauseFor(Duration::from_hours(1))
        );
        let evening = at(&central_europe(), 2026, 10, 5, 22, 0);
        assert_eq!(
            MenuAction::Pause(PausePreset::UntilMorning).command(|| evening),
            TrayCommand::PauseFor(hours(10.0))
        );
    }

    #[test]
    fn morning_pause_ends_at_eight() {
        let tz = central_europe();
        for ((hour, minute), expected) in [
            ((22, 0), 10.0),
            ((23, 59), 8.0 + 1.0 / 60.0),
            ((0, 0), 8.0),
            ((2, 0), 6.0),
            ((3, 59), 4.0 + 1.0 / 60.0),
            ((4, 0), 28.0),
            ((7, 59), 24.0 + 1.0 / 60.0),
            ((8, 0), 24.0),
            ((12, 30), 19.5),
        ] {
            let now = at(&tz, 2026, 10, 7, hour, minute);
            assert_eq!(
                morning_pause(&now),
                hours(expected),
                "{hour:02}:{minute:02}"
            );
        }
    }

    #[test]
    fn morning_pause_follows_daylight_saving_changes() {
        let tz = central_europe();
        // Clocks go forward on 2026-03-29 at 02:00: one hour less.
        let now = at(&tz, 2026, 3, 28, 22, 0);
        assert_eq!(morning_pause(&now), hours(9.0));
        // Clocks go back on 2026-10-25 at 03:00: one hour more.
        let now = at(&tz, 2026, 10, 24, 22, 0);
        assert_eq!(morning_pause(&now), hours(11.0));
        // In UTC nothing shifts.
        let now = at(&TimeZone::UTC, 2026, 3, 28, 22, 0);
        assert_eq!(morning_pause(&now), hours(10.0));
    }

    #[test]
    fn morning_pause_falls_back_at_the_end_of_time() {
        let now = at(&TimeZone::UTC, 9999, 12, 30, 12, 0);
        assert_eq!(morning_pause(&now), FALLBACK_PAUSE);
    }

    #[test]
    fn fixed_pauses_ignore_the_clock() {
        let never = || -> Zoned { panic!("the clock is not needed") };
        assert_eq!(
            PausePreset::HalfHour.duration(never),
            Duration::from_mins(30)
        );
        assert_eq!(PausePreset::Hour.duration(never), Duration::from_hours(1));
    }

    fn labels(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Item { label, .. } => label.clone(),
                Entry::Choice { label, current, .. } => {
                    format!("{}{label}", if *current { "✓ " } else { "  " })
                }
                Entry::Status(text) => format!("({text})"),
                Entry::Submenu { label, entries, .. } => {
                    format!("{label} ▸ {}", labels(entries).join(" / "))
                }
                Entry::Separator => "---".to_owned(),
            })
            .collect()
    }

    #[test]
    fn default_menu() {
        assert_eq!(
            labels(&entries(&state())),
            [
                "Open icygui",
                "---",
                "Pause notifications ▸ For 30 minutes / For 1 hour / Until tomorrow morning",
                "---",
                "Quit icygui",
            ]
        );
    }

    #[test]
    fn paused_menu_shows_the_end_and_resume() {
        let mut state = state();
        state.paused_until = Some("18:30".to_owned());
        let entries = entries(&state);
        assert_eq!(
            labels(&entries),
            [
                "Open icygui",
                "---",
                "(Paused until 18:30)",
                "Resume notifications",
                "Pause notifications ▸ For 30 minutes / For 1 hour / Until tomorrow morning",
                "---",
                "Quit icygui",
            ]
        );
        assert!(entries.contains(&Entry::Item {
            action: MenuAction::Resume,
            label: "Resume notifications".to_owned()
        }));
    }

    #[test]
    fn environments_submenu_marks_the_active_one() {
        let mut state = state();
        state.environments = vec![
            ("id-prod".to_owned(), "prod-cluster".to_owned()),
            ("id-staging".to_owned(), "staging".to_owned()),
            ("id-lab".to_owned(), "  ".to_owned()),
        ];
        state.active = Some("id-staging".to_owned());
        assert_eq!(
            labels(&entries(&state)),
            [
                "Open icygui",
                "---",
                "Pause notifications ▸ For 30 minutes / For 1 hour / Until tomorrow morning",
                "---",
                "Environment ▸   prod-cluster / ✓ staging /   id-lab",
                "---",
                "Quit icygui",
            ]
        );

        // An unknown active id checks nothing.
        state.active = Some("gone".to_owned());
        let shown = labels(&entries(&state));
        assert!(!shown.iter().any(|label| label.contains('✓')), "{shown:?}");
    }

    #[test]
    fn environment_items_switch_by_id() {
        let mut state = state();
        state.environments = vec![("3f2a".to_owned(), "prod".to_owned())];
        let entries = entries(&state);
        let Some(Entry::Submenu { entries: items, .. }) = entries.get(4) else {
            panic!("no environment submenu: {entries:?}");
        };
        assert_eq!(
            items,
            &[Entry::Choice {
                action: MenuAction::SwitchEnvironment("3f2a".to_owned()),
                label: "prod".to_owned(),
                current: false,
            }]
        );
    }

    #[test]
    fn labels_are_single_short_lines() {
        assert_eq!(clean_label("  prod\ncluster\t1 "), "prod cluster 1");
        assert_eq!(clean_label("\u{1b}[31mred"), "[31mred");
        assert_eq!(clean_label("R&D"), "R&D");
        let long = "x".repeat(200);
        let short = clean_label(&long);
        assert_eq!(short.chars().count(), MAX_LABEL_CHARS);
        assert!(short.ends_with('…'));
        let words = format!("{} tail", "word ".repeat(30));
        assert!(!clean_label(&words).ends_with(" …"));
        let wide = "日本".repeat(40);
        assert_eq!(clean_label(&wide).chars().count(), MAX_LABEL_CHARS);
        assert_eq!(environment_label("", ""), "(unnamed)");
        assert_eq!(environment_label("id\n1", " "), "id 1");
    }

    #[test]
    fn ampersands_are_escaped_for_muda() {
        assert_eq!(muda_text("R&D && ops"), "R&&D &&&& ops");
        assert_eq!(muda_text("plain_name"), "plain_name");
    }

    /// The menu as the Linux tray host sees it (muda's snapshot, which the
    /// ksni backend turns into a D-Bus menu).
    #[cfg(target_os = "linux")]
    mod rendered {
        use tray_icon::menu::{ContextMenu as _, MenuItemKindSnapshot};

        use super::*;

        pub(super) fn shown(items: &[MenuItemKindSnapshot]) -> Vec<String> {
            items
                .iter()
                .map(|item| match item {
                    MenuItemKindSnapshot::MenuItem(item) => {
                        format!(
                            "{}{}",
                            item.text(),
                            if item.is_enabled() { "" } else { " (off)" }
                        )
                    }
                    MenuItemKindSnapshot::Check(item) => format!(
                        "[{}] {}{}",
                        if item.is_checked() { "x" } else { " " },
                        item.text(),
                        if item.is_enabled() { "" } else { " (off)" }
                    ),
                    MenuItemKindSnapshot::Submenu(item) => {
                        format!("{} ▸ {}", item.text(), shown(&item.items()).join(" / "))
                    }
                    MenuItemKindSnapshot::Predefined(item) if item.is_separator() => {
                        "---".to_owned()
                    }
                    _ => panic!("the tray menu has no icon or predefined items"),
                })
                .collect()
        }

        #[test]
        fn built_menu_matches_the_model() {
            let mut state = state();
            state.app_name = "icygui".to_owned();
            state.paused_until = Some("tomorrow 08:00".to_owned());
            state.environments = vec![
                ("a".to_owned(), "R&D".to_owned()),
                ("b".to_owned(), "prod_cluster".to_owned()),
            ];
            state.active = Some("b".to_owned());
            let menu = build(&entries(&state)).unwrap();
            assert_eq!(
                shown(&menu.snapshot_handle().items()),
                [
                    "Open icygui",
                    "---",
                    "Paused until tomorrow 08:00 (off)",
                    "Resume notifications",
                    "Pause notifications ▸ For 30 minutes / For 1 hour / Until tomorrow morning",
                    "---",
                    "Environment ▸ [ ] R&&D / [x] prod_cluster (off)",
                    "---",
                    "Quit icygui",
                ]
            );
        }

        #[test]
        fn built_items_carry_the_action_ids() {
            let mut state = state();
            state.environments = vec![("env-1".to_owned(), "prod".to_owned())];
            state.paused_until = Some("18:30".to_owned());
            let menu = build(&entries(&state)).unwrap();
            let ids: Vec<String> = menu
                .items()
                .iter()
                .flat_map(|item| {
                    let mut ids = vec![item.id().0.clone()];
                    if let Some(submenu) = item.as_submenu() {
                        ids.extend(submenu.items().iter().map(|child| child.id().0.clone()));
                    }
                    ids
                })
                .filter(|id| id.starts_with(PREFIX))
                .collect();
            assert_eq!(
                ids,
                [
                    OPEN_ID,
                    STATUS_ID,
                    RESUME_ID,
                    PAUSE_MENU_ID,
                    "icygui.tray.pause.30m",
                    "icygui.tray.pause.1h",
                    "icygui.tray.pause.tomorrow",
                    ENVIRONMENT_MENU_ID,
                    "icygui.tray.environment.env-1",
                    QUIT_ID,
                ]
            );
        }
    }
}
