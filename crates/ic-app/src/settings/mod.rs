//! The settings dialog (`secondary-,`, the macOS app menu's *Settings*,
//! the palette; PLAN.md §2.8), a modal with two tabs:
//!
//! - *general*: keep running in the tray when the window closes (BG-01,
//!   with whether this desktop shows tray icons), launch at login in the
//!   background (BG-03), the event log's retention and the reconcile
//!   interval;
//! - *notifications* for the active environment (NOTE-02..06): the master
//!   switch and pausing (30 minutes, an hour, until 08:00, resume), the
//!   default rule with every condition (states, hard only, skip handled,
//!   acknowledgement, downtime and flapping events, minimum duration,
//!   sound), every group's and dashboard's setting (inherit, on, off, or a
//!   custom rule of its own), quiet hours (crossing midnight, by day,
//!   critical and down allowed), storm control, and the watched and muted
//!   objects.
//!
//! Changes are a draft until *save* (`secondary-s`, or Enter in a field);
//! Escape or *cancel* drops them. Pausing acts at once (it is not a
//! setting). Fields are checked when saving; problems show under them and
//! the keyboard goes to the first. Everything stays on this computer
//! (D6): Icinga's own notification switches are never touched.

pub(crate) mod about;
mod model;

use std::collections::BTreeMap;

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, InteractiveElement as _, IntoElement, KeyBinding,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window, div,
    prelude::FluentBuilder as _, px,
};
use ic_config::General;
use ic_model::Timestamp;
use ic_rules::{ObjectMode, Rule, ScopeSetting};
use ic_ui_kit::input::{InputEvent, InputState};
use ic_ui_kit::{
    ActiveTheme as _, Button, CHIP_HEIGHT, Chip, DialogBody, Field, Link, Segmented, Switch,
    TextField, Theme, Tooltip,
};

pub(crate) use self::model::{FieldId, RuleFlag, ScopeKey, SettingsTab};
use self::model::{
    SCOPE_CHOICES, format_clock, format_min_duration, parse_clock, parse_min_duration,
    parse_reconcile, parse_retention, parse_threshold, parse_window, scope_choice, scope_meaning,
};
use crate::app_state::{AppState, NotificationPlan};
use crate::notifications::{PauseChoice, override_text, pause_label, paused_text};
use crate::operate::dialog::{NextField, PreviousField};

/// Key context of the dialog.
pub(crate) const SETTINGS_CONTEXT: &str = "SettingsDialog";

/// The default reconcile interval offered when switching from adaptive.
const FIXED_RECONCILE_DEFAULT: u32 = 600;

/// The weekdays, Monday first, as quiet hours' day chips show them.
const DAYS: [&str; 7] = ["mo", "tu", "we", "th", "fr", "sa", "su"];

/// Saves the settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct SaveSettings;

/// Registers the dialog's keys: `secondary-s` saves, Enter in a field
/// saves, Tab and Shift-Tab move between the fields.
pub(crate) fn bind_keys(cx: &mut App) {
    let field = Some("SettingsDialog > Input");
    let dialog = Some(SETTINGS_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("secondary-s", SaveSettings, dialog),
        KeyBinding::new("enter", SaveSettings, field),
        KeyBinding::new("tab", NextField, field),
        KeyBinding::new("shift-tab", PreviousField, field),
        KeyBinding::new("tab", NextField, dialog),
        KeyBinding::new("shift-tab", PreviousField, dialog),
    ]);
}

/// The settings shortcut as menus and the palette show it.
pub(crate) fn settings_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘,"
    } else {
        "ctrl-,"
    }
}

/// The quit shortcut as menus and the palette show it.
pub(crate) fn quit_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘Q"
    } else {
        "ctrl-q"
    }
}

/// What the dialog asks its owner to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SettingsEvent {
    /// Close it (cancelled, or saved).
    Close,
    /// Saved: launch at login changed to this (the owner writes the login
    /// entry off the UI thread).
    LaunchAtLogin(bool),
    /// Show the about dialog.
    About,
}

/// The settings dialog.
pub(crate) struct SettingsDialog {
    state: Entity<AppState>,
    tab: SettingsTab,
    general: General,
    /// Reconciles adaptively (`reconcile_interval_secs` 0).
    adaptive: bool,
    /// The active environment's notification settings (`None` without an
    /// environment).
    plan: Option<NotificationPlan>,
    /// The active environment's name.
    environment: String,
    inputs: BTreeMap<FieldId, Entity<InputState>>,
    errors: BTreeMap<FieldId, String>,
    /// Saving was tried: problems update as the fields change.
    tried: bool,
    /// Whether this desktop shows tray icons (`None` while asking).
    tray_host: Option<bool>,
    demo: bool,
    focus_handle: FocusHandle,
    subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<SettingsEvent> for SettingsDialog {}

impl Focusable for SettingsDialog {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl SettingsDialog {
    /// The dialog on `tab`. With `custom`, that group or dashboard starts
    /// with a custom rule of its own (the sidebar's *custom rule*).
    pub(crate) fn new(
        state: Entity<AppState>,
        tab: SettingsTab,
        custom: Option<&ScopeKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (general, mut plan, environment, demo) = {
            let state = state.read(cx);
            (
                state.config().general.clone(),
                state.notification_plan(),
                state
                    .environment()
                    .map(|environment| environment.name.clone())
                    .unwrap_or_default(),
                state.is_demo(),
            )
        };
        if let (Some(plan), Some(key)) = (plan.as_mut(), custom) {
            plan.choose(key, 3);
        }
        let adaptive = general.reconcile_interval_secs == 0;
        let host_check = cx
            .background_executor()
            .spawn(async { ic_platform::tray::host_available() });
        let tray_task = cx.spawn(async move |this, cx| {
            let available = host_check.await;
            let _ = this.update(cx, |this, cx| {
                this.tray_host = Some(available);
                cx.notify();
            });
        });
        let mut dialog = Self {
            state,
            tab,
            general: general.clone(),
            adaptive,
            plan,
            environment,
            inputs: BTreeMap::new(),
            errors: BTreeMap::new(),
            tried: false,
            tray_host: None,
            demo,
            focus_handle: cx.focus_handle(),
            subscriptions: Vec::new(),
            _tasks: vec![tray_task],
        };
        dialog.add_input(
            FieldId::Retention,
            general.event_log_retention_hours.to_string(),
            "48",
            window,
            cx,
        );
        let reconcile = if adaptive {
            String::new()
        } else {
            general.reconcile_interval_secs.to_string()
        };
        dialog.add_input(
            FieldId::Reconcile,
            reconcile,
            &FIXED_RECONCILE_DEFAULT.to_string(),
            window,
            cx,
        );
        if let Some(plan) = dialog.plan.clone() {
            let quiet = plan.settings.quiet_hours;
            let storm = plan.settings.storm;
            dialog.add_input(
                FieldId::QuietStart,
                format_clock(quiet.start_minute),
                "22:00",
                window,
                cx,
            );
            dialog.add_input(
                FieldId::QuietEnd,
                format_clock(quiet.end_minute),
                "07:00",
                window,
                cx,
            );
            dialog.add_input(
                FieldId::StormThreshold,
                storm.threshold.to_string(),
                "5",
                window,
                cx,
            );
            dialog.add_input(
                FieldId::StormWindow,
                storm.window_secs.to_string(),
                "10",
                window,
                cx,
            );
            for key in plan.rule_scopes() {
                dialog.add_rule_input(&key, window, cx);
            }
        }
        dialog
    }

    /// Adds a text field.
    fn add_input(
        &mut self,
        id: FieldId,
        value: String,
        placeholder: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder.to_owned())
                .default_value(value)
        });
        self.subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) && this.tried {
                    this.errors = this.collect(cx).err().unwrap_or_default();
                    cx.notify();
                }
            },
        ));
        self.inputs.insert(id, input);
    }

    /// Adds the minimum-duration field of a scope's rule, unless it has one.
    fn add_rule_input(&mut self, key: &ScopeKey, window: &mut Window, cx: &mut Context<Self>) {
        let id = FieldId::MinDuration(key.clone());
        if self.inputs.contains_key(&id) {
            return;
        }
        let seconds = self
            .plan
            .as_ref()
            .and_then(|plan| plan.rule(key))
            .map_or(0, |rule| rule.min_duration_secs);
        self.add_input(id, format_min_duration(seconds), "0", window, cx);
    }

    /// Where the keyboard goes when the dialog opens.
    pub(crate) fn default_focus(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// The tab shown.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn tab(&self) -> SettingsTab {
        self.tab
    }

    /// The problems shown, by field.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn errors(&self) -> &BTreeMap<FieldId, String> {
        &self.errors
    }

    /// A text field, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn input(&self, id: &FieldId) -> Option<&Entity<InputState>> {
        self.inputs.get(id)
    }

    /// Whether this desktop shows tray icons, once known.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn tray_host(&self) -> Option<bool> {
        self.tray_host
    }

    /// The notification draft, for tests.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn plan(&self) -> Option<&NotificationPlan> {
        self.plan.as_ref()
    }

    /// Shows `tab`.
    pub(crate) fn show_tab(&mut self, tab: SettingsTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.notify();
        }
    }

    /// Turns a rule condition on or off.
    pub(crate) fn set_flag(
        &mut self,
        key: &ScopeKey,
        flag: RuleFlag,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        if let Some(rule) = self.plan.as_mut().and_then(|plan| plan.rule_mut(key)) {
            flag.set(rule, on);
            cx.notify();
        }
    }

    /// Turns quiet hours on or off.
    pub(crate) fn set_quiet_hours(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(plan) = self.plan.as_mut() {
            plan.settings.quiet_hours.enabled = on;
            cx.notify();
        }
    }

    /// Chooses a group's or dashboard's setting.
    pub(crate) fn choose_scope(
        &mut self,
        key: &ScopeKey,
        choice: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self
            .plan
            .as_mut()
            .is_some_and(|plan| plan.choose(key, choice));
        if changed {
            if choice == 3 {
                self.add_rule_input(key, window, cx);
            }
            cx.notify();
        }
    }

    /// The text of a field.
    fn text(&self, id: &FieldId, cx: &App) -> String {
        self.inputs
            .get(id)
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// The settings as edited, or the problems by field.
    fn collect(
        &self,
        cx: &App,
    ) -> Result<(General, Option<NotificationPlan>), BTreeMap<FieldId, String>> {
        let mut errors = BTreeMap::new();
        let mut general = self.general.clone();
        match parse_retention(&self.text(&FieldId::Retention, cx)) {
            Ok(hours) => general.event_log_retention_hours = hours,
            Err(error) => {
                errors.insert(FieldId::Retention, error);
            }
        }
        if self.adaptive {
            general.reconcile_interval_secs = 0;
        } else {
            let text = self.text(&FieldId::Reconcile, cx);
            let text = if text.trim().is_empty() {
                FIXED_RECONCILE_DEFAULT.to_string()
            } else {
                text
            };
            match parse_reconcile(&text) {
                Ok(seconds) => general.reconcile_interval_secs = seconds,
                Err(error) => {
                    errors.insert(FieldId::Reconcile, error);
                }
            }
        }
        let mut plan = self.plan.clone();
        if let Some(plan) = plan.as_mut() {
            let mut check = |id: FieldId, parsed: Result<u32, String>| match parsed {
                Ok(value) => Some(value),
                Err(error) => {
                    errors.insert(id, error);
                    None
                }
            };
            let quiet = &mut plan.settings.quiet_hours;
            if let Some(minute) = check(
                FieldId::QuietStart,
                parse_clock(&self.text(&FieldId::QuietStart, cx)).map(u32::from),
            ) {
                quiet.start_minute = u16::try_from(minute).unwrap_or_default();
            }
            if let Some(minute) = check(
                FieldId::QuietEnd,
                parse_clock(&self.text(&FieldId::QuietEnd, cx)).map(u32::from),
            ) {
                quiet.end_minute = u16::try_from(minute).unwrap_or_default();
            }
            if let Some(threshold) = check(
                FieldId::StormThreshold,
                parse_threshold(&self.text(&FieldId::StormThreshold, cx)),
            ) {
                plan.settings.storm.threshold = threshold;
            }
            if let Some(window) = check(
                FieldId::StormWindow,
                parse_window(&self.text(&FieldId::StormWindow, cx)),
            ) {
                plan.settings.storm.window_secs = window;
            }
            for key in plan.rule_scopes() {
                let id = FieldId::MinDuration(key.clone());
                if let Some(seconds) = check(id.clone(), parse_min_duration(&self.text(&id, cx)))
                    && let Some(rule) = plan.rule_mut(&key)
                {
                    rule.min_duration_secs = seconds;
                }
            }
        }
        if errors.is_empty() {
            Ok((general, plan))
        } else {
            Err(errors)
        }
    }

    /// Saves, or shows what's wrong and moves the keyboard there.
    pub(crate) fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tried = true;
        let (general, plan) = match self.collect(cx) {
            Ok(collected) => collected,
            Err(errors) => {
                let first = errors.keys().next().cloned();
                // A problem on the other tab: show that tab.
                if let Some(first) = &first {
                    self.tab = match first {
                        FieldId::Retention | FieldId::Reconcile => SettingsTab::General,
                        _ => SettingsTab::Notifications,
                    };
                    if let Some(input) = self.inputs.get(first) {
                        input.focus_handle(cx).focus(window, cx);
                    }
                }
                self.errors = errors;
                cx.notify();
                return;
            }
        };
        self.errors.clear();
        let login = (general.launch_at_login
            != self.state.read(cx).config().general.launch_at_login)
            .then_some(general.launch_at_login);
        self.state.update(cx, |state, cx| {
            let mut changed = state.set_general(&general);
            if let Some(plan) = plan {
                changed |= state.apply_notification_plan(plan);
            }
            if changed {
                state.inform("Settings saved", None);
            }
            cx.notify();
        });
        if let Some(enabled) = login {
            cx.emit(SettingsEvent::LaunchAtLogin(enabled));
        }
        cx.emit(SettingsEvent::Close);
    }

    fn on_save(&mut self, _: &SaveSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.save(window, cx);
    }

    /// The text fields shown on the current tab, in order.
    fn visible_inputs(&self) -> Vec<Entity<InputState>> {
        let ids: Vec<FieldId> = match self.tab {
            SettingsTab::General => {
                let mut ids = vec![FieldId::Retention];
                if !self.adaptive {
                    ids.push(FieldId::Reconcile);
                }
                ids
            }
            SettingsTab::Notifications => {
                let Some(plan) = &self.plan else {
                    return Vec::new();
                };
                let mut ids: Vec<FieldId> = plan
                    .rule_scopes()
                    .into_iter()
                    .map(FieldId::MinDuration)
                    .collect();
                if plan.settings.quiet_hours.enabled {
                    ids.extend([FieldId::QuietStart, FieldId::QuietEnd]);
                }
                ids.extend([FieldId::StormThreshold, FieldId::StormWindow]);
                ids
            }
        };
        ids.iter()
            .filter_map(|id| self.inputs.get(id).cloned())
            .collect()
    }

    fn move_focus(&self, forward: bool, window: &mut Window, cx: &mut App) {
        let inputs = self.visible_inputs();
        if inputs.is_empty() {
            return;
        }
        let current = inputs
            .iter()
            .position(|input| input.focus_handle(cx).is_focused(window));
        let count = inputs.len();
        let next = match (current, forward) {
            (Some(index), true) => (index + 1) % count,
            (Some(index), false) => (index + count - 1) % count,
            (None, true) => 0,
            (None, false) => count - 1,
        };
        inputs[next].focus_handle(cx).focus(window, cx);
    }

    fn on_next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(true, window, cx);
    }

    fn on_previous_field(
        &mut self,
        _: &PreviousField,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.move_focus(false, window, cx);
    }

    // --- Rendering --------------------------------------------------------

    /// A text field with its label, problem and hint.
    fn field(
        &self,
        label: &'static str,
        id: &FieldId,
        width: f32,
        hint: Option<&'static str>,
    ) -> Field {
        let error = self.errors.get(id).cloned();
        let mut field = Field::new(label);
        if let Some(input) = self.inputs.get(id) {
            field = field.control(
                div().w(px(width)).child(
                    TextField::new(input)
                        .bordered(true)
                        .invalid(error.is_some()),
                ),
            );
        }
        let field = field.error(error);
        match hint {
            Some(hint) => field.hint(hint),
            None => field,
        }
    }

    /// A text field inside a sentence (storm control), with no label.
    fn inline_field(&self, id: &FieldId, width: f32) -> AnyElement {
        let invalid = self.errors.contains_key(id);
        match self.inputs.get(id) {
            Some(input) => div()
                .w(px(width))
                .child(TextField::new(input).bordered(true).invalid(invalid))
                .into_any_element(),
            None => div().into_any_element(),
        }
    }

    fn render_general(&self, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        let mut blocks = self.render_background(theme, cx);
        blocks.extend(self.render_engine(theme, cx));
        blocks
    }

    /// The tray, launch at login and quiet mode (BG-01, BG-03, PERF-09).
    fn render_background(&self, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        let tray_hint = match self.tray_host {
            None => "Checking whether this desktop shows tray icons",
            Some(true) => {
                "The tray icon shows the worst unhandled state; its menu opens the window, \
                 pauses notifications, switches environments and quits."
            }
            Some(false) => {
                "This desktop shows no tray icons (stock GNOME needs the AppIndicator \
                 extension): closing the window quits icygui."
            }
        };
        let login_hint = if self.demo {
            "Not in the demo: it would start the demo at every login."
        } else {
            "Starts icygui in the tray, without its window, when you log in."
        };
        vec![
            div()
                .text_size(theme.text.label)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.colors.text_muted)
                .child("in the background")
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    Switch::new("settings-close-to-tray", self.general.close_to_tray)
                        .label("keep running in the tray when the window closes")
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            this.general.close_to_tray = *on;
                            cx.notify();
                        })),
                )
                .child(hint(tray_hint, theme))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    Switch::new("settings-launch-at-login", self.general.launch_at_login)
                        .label("start at login")
                        .disabled(self.demo)
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            this.general.launch_at_login = *on;
                            cx.notify();
                        })),
                )
                .child(hint(login_hint, theme))
                .into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    Switch::new("settings-quiet-mode", self.general.quiet_when_hidden)
                        .label("quiet mode when hidden")
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            this.general.quiet_when_hidden = *on;
                            cx.notify();
                        })),
                )
                .child(hint(
                    "Environments off screen, and the one on screen once the window has \
                     been closed, minimised or otherwise out of sight for half a minute, \
                     follow Icinga without check results: far less load on the master, \
                     notifications as prompt as ever; outputs catch up when you look.",
                    theme,
                ))
                .into_any_element(),
        ]
    }

    /// The event log, reconciles, and the version.
    fn render_engine(&self, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        let colors = theme.colors;
        vec![
            section(theme, "event log").into_any_element(),
            self.field(
                "keep events for (hours)",
                &FieldId::Retention,
                120.,
                Some("The history tabs and the notification centre read the local log."),
            )
            .into_any_element(),
            section(theme, "reconcile with Icinga").into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div().w(px(320.)).child(
                        Segmented::new("settings-reconcile")
                            .option("adaptive")
                            .option("fixed interval")
                            .selected(usize::from(!self.adaptive))
                            .on_select(cx.listener(|this, index: &usize, window, cx| {
                                this.adaptive = *index == 0;
                                if !this.adaptive
                                    && let Some(input) = this.inputs.get(&FieldId::Reconcile)
                                {
                                    input.focus_handle(cx).focus(window, cx);
                                }
                                cx.notify();
                            })),
                    ),
                )
                .when(!self.adaptive, |column| {
                    column.child(self.field(
                        "every (seconds)",
                        &FieldId::Reconcile,
                        120.,
                        Some("At least 60 seconds."),
                    ))
                })
                .child(hint(
                    "A lean reload of every object catches what the event stream missed. \
                     Adaptive: from every 5 minutes for a small Icinga to every 15 at \
                     30 000 objects, up to an hour while the stream runs without a break.",
                    theme,
                ))
                .into_any_element(),
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .pt(px(4.))
                .text_size(theme.text.small)
                .text_color(colors.text_faint)
                .child(format!("icygui {}", env!("CARGO_PKG_VERSION")))
                .child(
                    Link::new("settings-about", "about")
                        .quiet()
                        .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                            cx.emit(SettingsEvent::About);
                        })),
                )
                .into_any_element(),
        ]
    }

    /// The editor of one rule: states, events, switches, minimum duration.
    fn rule_editor(
        &self,
        key: &ScopeKey,
        rule: &Rule,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let prefix = scope_id(key);
        let chip = |flag: RuleFlag| {
            let key = key.clone();
            let on = flag.get(rule);
            Chip::new(
                SharedString::from(format!("{prefix}-{}", flag.label())),
                flag.label(),
            )
            .selected(on)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_flag(&key, flag, !on, cx);
            }))
        };
        let switch = |flag: RuleFlag| {
            let key = key.clone();
            Switch::new(
                SharedString::from(format!("{prefix}-{flag:?}")),
                flag.get(rule),
            )
            .label(flag.label())
            .on_change(cx.listener(move |this, on: &bool, _, cx| {
                this.set_flag(&key, flag, *on, cx);
            }))
        };
        let label = |text: &'static str| {
            div()
                .w(px(64.))
                .flex_none()
                .text_size(theme.text.small)
                .text_color(theme.colors.text_faint)
                .child(text)
        };
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(label("states"))
                    .children(RuleFlag::STATES.map(chip)),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(label("events"))
                    .children(RuleFlag::EVENTS.map(chip)),
            )
            .child(switch(RuleFlag::HardOnly))
            .child(switch(RuleFlag::SkipHandled))
            .child(switch(RuleFlag::Sound))
            .child(self.field(
                "only after",
                &FieldId::MinDuration(key.clone()),
                120.,
                Some(
                    "A problem must last this long first (5m, 1h; 0 = at once); it is \
                     dropped if it recovers or is handled before.",
                ),
            ))
            .into_any_element()
    }

    /// A group's or dashboard's setting, and its custom rule.
    fn scope_row(&self, row: &ScopeRow<'_>, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let ScopeRow {
            key,
            name,
            setting,
            parent,
            indent,
        } = *row;
        let choose = key.clone();
        let rule = match setting {
            ScopeSetting::Custom(rule) => Some(rule),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .pl(px(indent))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .when(indent == 0., |name| name.font_weight(FontWeight::MEDIUM))
                                    .text_color(theme.colors.text_strong)
                                    .child(name.to_owned()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(theme.text.small)
                                    .text_color(theme.colors.text_faint)
                                    .child(scope_meaning(setting, parent)),
                            ),
                    )
                    .child(
                        div().w(px(280.)).flex_none().child(
                            SCOPE_CHOICES
                                .iter()
                                .fold(
                                    Segmented::new(SharedString::from(format!(
                                        "{}-setting",
                                        scope_id(key)
                                    ))),
                                    |control, choice| control.option(*choice),
                                )
                                .selected(scope_choice(setting))
                                .on_select(cx.listener(move |this, index: &usize, window, cx| {
                                    this.choose_scope(&choose, *index, window, cx);
                                })),
                        ),
                    ),
            )
            .when_some(rule, |row, rule| {
                row.child(
                    div()
                        .ml(px(4.))
                        .pl(px(14.))
                        .py(px(4.))
                        .border_l_2()
                        .border_color(theme.colors.border_header)
                        .child(self.rule_editor(key, rule, theme, cx)),
                )
            })
            .into_any_element()
    }

    fn render_notifications(&self, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(plan) = &self.plan else {
            return vec![
                div()
                    .text_color(theme.colors.text_muted)
                    .child("Add an environment first: notification rules belong to an environment.")
                    .into_any_element(),
            ];
        };
        let mut blocks = vec![
            self.render_master_switch(plan, theme, cx),
            section(theme, "default rule").into_any_element(),
            hint(
                "Groups and dashboards notify with it unless they say otherwise. Recoveries \
                 notify only for problems that notified.",
                theme,
            )
            .into_any_element(),
            self.rule_editor(
                &ScopeKey::Environment,
                &plan.settings.default_rule,
                theme,
                cx,
            ),
            section(theme, "groups and dashboards").into_any_element(),
        ];
        blocks.extend(self.render_scopes(plan, theme, cx));
        blocks.extend(self.render_quiet_hours(plan, theme, cx));
        blocks.extend(self.render_storm(theme));
        blocks.extend(Self::render_overrides(plan, Timestamp::now(), theme, cx));
        blocks
    }

    /// The environment's master switch, and pausing (acts at once).
    fn render_master_switch(
        &self,
        plan: &NotificationPlan,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let now = Timestamp::now();
        let environments = self.state.read(cx).environments().len();
        let paused = self
            .state
            .read(cx)
            .paused_until()
            .filter(|until| *until > now);
        let pause: AnyElement = match paused {
            Some(until) => div()
                .flex()
                .items_center()
                .gap(px(8.))
                .text_size(theme.text.small)
                .text_color(theme.states.warning)
                .child(div().line_height(px(CHIP_HEIGHT)).child(paused_text(
                    environments,
                    until,
                    now,
                )))
                .child(Link::new("settings-resume", "resume").on_click(cx.listener(
                    |this, _: &ClickEvent, _, cx| {
                        this.state.update(cx, |state, cx| {
                            state.pause_notifications(None);
                            cx.notify();
                        });
                    },
                )))
                .into_any_element(),
            None => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .text_size(theme.text.small)
                        .text_color(colors.text_faint)
                        .child(pause_label(environments)),
                )
                .children(PauseChoice::ALL.map(|choice| {
                    Chip::new(
                        SharedString::from(format!("settings-pause-{choice:?}")),
                        choice.short_label(),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                state.pause_notifications(Some(choice.until(Timestamp::now())));
                                cx.notify();
                            });
                        },
                    ))
                }))
                .into_any_element(),
        };
        div()
            .flex()
            .items_center()
            .gap(px(12.))
            .child(
                div().flex_1().child(
                    Switch::new("settings-notifications-enabled", plan.settings.enabled)
                        .label(format!("notifications for {}", self.environment))
                        .on_change(cx.listener(|this, on: &bool, _, cx| {
                            if let Some(plan) = this.plan.as_mut() {
                                plan.settings.enabled = *on;
                            }
                            cx.notify();
                        })),
                ),
            )
            .child(pause)
            .into_any_element()
    }

    /// Every group's and dashboard's setting.
    fn render_scopes(
        &self,
        plan: &NotificationPlan,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let mut blocks = Vec::new();
        if plan.groups.is_empty() {
            blocks.push(hint("No dashboards yet.", theme).into_any_element());
        }
        let environment = self.environment.clone();
        for group in &plan.groups {
            let key = ScopeKey::Group(group.id.clone());
            blocks.push(self.scope_row(
                &ScopeRow {
                    key: &key,
                    name: &group.name,
                    setting: &group.setting,
                    parent: &environment,
                    indent: 0.,
                },
                theme,
                cx,
            ));
            let parent = group.name.clone();
            for (id, name, setting) in &group.dashboards {
                blocks.push(self.scope_row(
                    &ScopeRow {
                        key: &ScopeKey::Dashboard(group.id.clone(), id.clone()),
                        name,
                        setting,
                        parent: &parent,
                        indent: 20.,
                    },
                    theme,
                    cx,
                ));
            }
        }
        blocks
    }

    fn render_quiet_hours(
        &self,
        plan: &NotificationPlan,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let quiet = plan.settings.quiet_hours;
        let mut blocks = vec![
            section(theme, "quiet hours").into_any_element(),
            Switch::new("settings-quiet", quiet.enabled)
                .label("record notifications silently at night")
                .on_change(cx.listener(|this, on: &bool, _, cx| {
                    this.set_quiet_hours(*on, cx);
                }))
                .into_any_element(),
        ];
        if quiet.enabled {
            blocks.push(
                div()
                    .flex()
                    .gap(px(16.))
                    .child(self.field("from", &FieldId::QuietStart, 100., None))
                    .child(self.field("to", &FieldId::QuietEnd, 100., None))
                    .into_any_element(),
            );
            blocks.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .w(px(64.))
                            .text_size(theme.text.small)
                            .text_color(theme.colors.text_faint)
                            .child("starting"),
                    )
                    .children(DAYS.iter().enumerate().map(|(index, day)| {
                        let on = quiet.days.get(index).copied().unwrap_or(false);
                        Chip::new(SharedString::from(format!("settings-day-{day}")), *day)
                            .selected(on)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                if let Some(plan) = this.plan.as_mut()
                                    && let Some(day) = plan.settings.quiet_hours.days.get_mut(index)
                                {
                                    *day = !on;
                                }
                                cx.notify();
                            }))
                    }))
                    .into_any_element(),
            );
            blocks.push(
                Switch::new("settings-quiet-critical", quiet.allow_critical)
                    .label("critical and down still notify out loud")
                    .on_change(cx.listener(|this, on: &bool, _, cx| {
                        if let Some(plan) = this.plan.as_mut() {
                            plan.settings.quiet_hours.allow_critical = *on;
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            blocks.push(
                hint(
                    "A window may cross midnight (22:00 to 07:00); the days are the ones it \
                     starts on. Quiet notifications still go to the notification centre.",
                    theme,
                )
                .into_any_element(),
            );
        }
        blocks
    }

    fn render_storm(&self, theme: &Theme) -> Vec<AnyElement> {
        let text = |text: &'static str| {
            div()
                .flex_none()
                .text_color(theme.colors.text_muted)
                .child(text)
        };
        let error = [FieldId::StormThreshold, FieldId::StormWindow]
            .iter()
            .find_map(|id| self.errors.get(id).cloned());
        vec![
            section(theme, "storm control").into_any_element(),
            div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(8.))
                        .child(text("at most"))
                        .child(self.inline_field(&FieldId::StormThreshold, 64.))
                        .child(text("notifications in"))
                        .child(self.inline_field(&FieldId::StormWindow, 64.))
                        .child(text("seconds, then one summary")),
                )
                .children(error.map(|error| {
                    div()
                        .text_size(theme.text.small)
                        .text_color(theme.states.critical)
                        .child(error)
                }))
                .into_any_element(),
        ]
    }

    fn render_overrides(
        plan: &NotificationPlan,
        now: Timestamp,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let current: Vec<_> = plan
            .settings
            .objects
            .iter()
            .filter(|entry| entry.until.is_none_or(|until| until > now))
            .collect();
        let mut blocks = vec![section(theme, "watched and muted").into_any_element()];
        if current.is_empty() {
            blocks.push(
                hint(
                    "Nothing is watched or muted. Watch or mute a host or service from its \
                     pane's ··· menu or the palette.",
                    theme,
                )
                .into_any_element(),
            );
            return blocks;
        }
        for (index, entry) in current.into_iter().enumerate() {
            let object = entry.object.clone();
            let color = match entry.mode {
                ObjectMode::Watch => theme.colors.accent,
                ObjectMode::Mute => theme.colors.text_faint,
            };
            blocks.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.colors.text)
                            .child(crate::operate::forms::describe_objects(
                                std::slice::from_ref(&entry.object),
                            )),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(theme.text.small)
                            .text_color(color)
                            .child(override_text(entry, now)),
                    )
                    .child(
                        Link::new(
                            SharedString::from(format!("settings-override-remove-{index}")),
                            "remove",
                        )
                        .quiet()
                        .tooltip(Tooltip::new("Notifications follow the dashboards again"))
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                if let Some(plan) = this.plan.as_mut() {
                                    plan.settings.objects.retain(|entry| entry.object != object);
                                }
                                cx.notify();
                            },
                        )),
                    )
                    .into_any_element(),
            );
        }
        blocks
    }
}

/// A group's or dashboard's row in the notification settings.
#[derive(Clone, Copy)]
struct ScopeRow<'a> {
    key: &'a ScopeKey,
    name: &'a str,
    setting: &'a ScopeSetting,
    /// What it inherits from: `the group databases`.
    parent: &'a str,
    indent: f32,
}

/// A section's heading: a rule above it, its name in the muted text.
fn section(theme: &Theme, label: &'static str) -> gpui::Div {
    div()
        .mt(px(4.))
        .pt(px(12.))
        .border_t_1()
        .border_color(theme.colors.border_row)
        .text_size(theme.text.label)
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme.colors.text_muted)
        .child(label)
}

/// A faint line of explanation.
fn hint(text: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .text_size(theme.text.small)
        .text_color(theme.colors.text_faint)
        .child(text.into())
}

/// A stable element id prefix for a scope.
fn scope_id(key: &ScopeKey) -> String {
    match key {
        ScopeKey::Environment => "rule-environment".to_owned(),
        ScopeKey::Group(id) => format!("rule-group-{id}"),
        ScopeKey::Dashboard(group, id) => format!("rule-dashboard-{group}-{id}"),
    }
}

impl Render for SettingsDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let title = div()
            .flex()
            .flex_1()
            .items_center()
            .gap(px(16.))
            .child("Settings")
            .child(div().flex_1())
            .child(
                div().w(px(260.)).font_weight(FontWeight::NORMAL).child(
                    SettingsTab::ALL
                        .iter()
                        .fold(Segmented::new("settings-tabs"), |control, tab| {
                            control.option(tab.label())
                        })
                        .selected(
                            SettingsTab::ALL
                                .iter()
                                .position(|tab| *tab == self.tab)
                                .unwrap_or(0),
                        )
                        .on_select(cx.listener(|this, index: &usize, _, cx| {
                            let tab = SettingsTab::ALL.get(*index).copied().unwrap_or_default();
                            this.show_tab(tab, cx);
                        })),
                ),
            );
        let blocks = match self.tab {
            SettingsTab::General => self.render_general(&theme, cx),
            SettingsTab::Notifications => self.render_notifications(&theme, cx),
        };
        let status = match (self.tab, self.demo) {
            (_, true) => "the demo saves nothing".to_owned(),
            (SettingsTab::General, false) => "for icygui on this computer".to_owned(),
            (SettingsTab::Notifications, false) => {
                format!("for {} · on this computer only", self.environment)
            }
        };
        let body = blocks
            .into_iter()
            .fold(DialogBody::new(title), DialogBody::child);
        div()
            .id("settings-dialog")
            .key_context(SETTINGS_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_save))
            .on_action(cx.listener(Self::on_next_field))
            .on_action(cx.listener(Self::on_previous_field))
            .max_h(px(760.))
            .flex()
            .flex_col()
            .child(
                body.footer_start(div().text_color(theme.colors.text_faint).child(status))
                    .action(
                        Button::new("settings-cancel", "cancel")
                            .key_hint("esc")
                            .on_click(cx.listener(|_, _: &ClickEvent, _, cx| {
                                cx.emit(SettingsEvent::Close);
                            })),
                    )
                    .action(
                        Button::new("settings-save", "save")
                            .primary()
                            .key_hint(if cfg!(target_os = "macos") {
                                "⌘S"
                            } else {
                                "ctrl-s"
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.save(window, cx);
                            })),
                    ),
            )
    }
}
