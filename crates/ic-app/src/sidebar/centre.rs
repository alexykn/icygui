//! The notification centre (NOTE-05, A2): opened from the footer's clock
//! icon, above it.
//!
//! - The heading counts what is unread in the scope; *mark all read* marks
//!   what the list shows (the scope, or what a label filter left).
//! - With several environments a row of tabs in the host pane's style
//!   picks the scope: one environment (the one on screen when the centre
//!   opens) or *all*. A scope with unread notifications shows its name in
//!   the accent colour (colour only: nothing moves); tabs that don't fit
//!   go into a `···` menu, so the row never wraps.
//! - The pause row pauses every environment, or shows that the one on
//!   screen is muted.
//! - The list, newest first, under `now`, `last hour`, `earlier today`,
//!   `yesterday`, `older`: unread entries have a bright title; silent ones
//!   a hollow dot, a dimmer title and why (`silent · storm`). A storm's
//!   notifications collapse into its summary (a click shows them). A
//!   click on an entry opens its object (switching to its environment)
//!   and marks it read; a click on its label (`overview`) shows only that
//!   place's, again shows everything.
//! - The footer says the history stays on this computer and for how long,
//!   and opens the notification settings.

use std::collections::HashSet;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px,
};
use gpui::{BoxShadow, point};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::Tone;
use ic_ui_kit::{
    ActiveTheme as _, CHIP_HEIGHT, Chip, Dismissable, Dismissal, Icon, IconName, Link, Menu,
    MenuItem, Popover, SUB_TAB_GAP, StateDot, SubTabs, Theme, Tooltip, sub_tab_width,
};

use super::{Sidebar, SidebarEvent, SidebarMenu};
use crate::app_state::AppState;
use crate::notifications::entry::{
    self, CentreEntry, CentreItem, CentreView, Place, Scope, Source, StormGroup,
};
use crate::notifications::{PauseChoice, pause_label, paused_text, when};
use crate::settings::SettingsTab;

/// The centre's width.
const CENTRE_WIDTH: f32 = 420.;
/// The centre's inner padding, left and right.
const CENTRE_PADDING: f32 = 14.;
/// The list's height before it scrolls.
const LIST_MAX_HEIGHT: f32 = 440.;
/// The longest environment name a scope tab shows in full.
const TAB_NAME_CHARS: usize = 20;
/// The scope overflow's trigger.
const MORE: &str = "···";

/// What the centre shows while it is open: its scope, a label filter, the
/// storms expanded, whether the scope overflow menu is open. Opening the
/// centre starts afresh (the environment on screen, everything).
#[derive(Debug, Default)]
pub(super) struct CentreState {
    /// The scope picked (`None`: the environment on screen).
    scope: Option<Scope>,
    /// The place a label click filters to.
    filter: Option<Place>,
    /// The storms shown expanded, by [`StormGroup::key`].
    expanded: HashSet<String>,
    /// Whether the scope tabs' `···` menu is open.
    overflow_open: bool,
}

/// The colour of a notification's tone.
pub(crate) fn tone_color(tone: Tone, theme: &Theme) -> gpui::Hsla {
    match tone {
        Tone::Critical => theme.states.critical,
        Tone::Warning => theme.states.warning,
        Tone::Unknown => theme.states.unknown,
        Tone::Recovery => theme.states.ok,
        Tone::Info => theme.colors.accent,
    }
}

/// A scope tab: what it selects and its label.
struct ScopeTab {
    scope: Scope,
    label: String,
    unread: bool,
}

/// Which of `widths` (tabs in order, the first always shown) fit in
/// `available` with `gap` between them, keeping `selected` among them; the
/// rest go behind a trigger `more` wide. Returns the shown and the hidden
/// tabs' indices, each in order.
pub(super) fn fit_tabs(
    widths: &[f32],
    selected: usize,
    available: f32,
    gap: f32,
    more: f32,
) -> (Vec<usize>, Vec<usize>) {
    // Each tab and the gap before it, but the first.
    let width_of = |shown: &[usize]| -> f32 {
        shown
            .iter()
            .enumerate()
            .map(|(place, index)| widths[*index] + if place == 0 { 0. } else { gap })
            .sum()
    };
    let every: Vec<usize> = (0..widths.len()).collect();
    if width_of(&every) <= available {
        return (every, Vec::new());
    }
    let budget = available - gap - more;
    let mut shown: Vec<usize> = vec![0];
    for index in 1..widths.len() {
        let mut next = shown.clone();
        next.push(index);
        if width_of(&next) > budget {
            break;
        }
        shown = next;
    }
    if !shown.contains(&selected) && selected < widths.len() {
        // The selected tab takes the place of the last ones.
        while shown.len() > 1 && width_of(&[shown.as_slice(), &[selected]].concat()) > budget {
            shown.pop();
        }
        shown.push(selected);
    }
    let hidden = (0..widths.len())
        .filter(|index| !shown.contains(index))
        .collect();
    (shown, hidden)
}

/// `name` short enough for a tab (`…` at the end).
fn tab_name(name: &str) -> String {
    if name.chars().count() <= TAB_NAME_CHARS {
        name.to_owned()
    } else {
        let mut short: String = name.chars().take(TAB_NAME_CHARS - 1).collect();
        short.push('…');
        short
    }
}

impl Sidebar {
    /// Opens the notification centre (the palette's *Notifications*).
    pub(crate) fn open_notifications(&mut self, cx: &mut Context<Self>) {
        self.centre = CentreState::default();
        self.menus.open(SidebarMenu::Notifications);
        cx.notify();
    }

    /// Opens or closes the centre from the footer's clock.
    pub(super) fn toggle_notifications(&mut self, down: Option<gpui::Point<gpui::Pixels>>) {
        if !self.menus.is_open(&SidebarMenu::Notifications) {
            self.centre = CentreState::default();
        }
        self.menus.toggle(SidebarMenu::Notifications, down);
    }

    /// Whether the notification centre is open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn notifications_open(&self) -> bool {
        self.menus.is_open(&SidebarMenu::Notifications)
    }

    /// The centre's scope.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn centre_scope(&self, cx: &gpui::App) -> Option<Scope> {
        self.scope(self.state.read(cx))
    }

    /// The centre's list as shown now.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn centre_view(&self, cx: &gpui::App) -> CentreView {
        self.view(self.state.read(cx), Timestamp::now())
    }

    /// The storms shown expanded.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn centre_expanded(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.centre.expanded.iter().cloned().collect();
        keys.sort();
        keys
    }

    /// The scope shown: the one picked while it still exists, else the
    /// environment on screen (`None` without environments).
    fn scope(&self, state: &AppState) -> Option<Scope> {
        let picked = self.centre.scope.clone().filter(|scope| match scope {
            Scope::All => state.environments().len() > 1,
            Scope::Environment(id) => state.environment_by_id(id).is_some(),
        });
        picked.or_else(|| {
            state
                .active_environment_id()
                .map(|id| Scope::Environment(id.to_owned()))
        })
    }

    /// The list for the scope and filter.
    fn view(&self, state: &AppState, now: Timestamp) -> CentreView {
        let Some(scope) = self.scope(state) else {
            return CentreView::default();
        };
        let sources: Vec<Source<'_>> = state
            .environments()
            .iter()
            .filter(|environment| match &scope {
                Scope::All => true,
                Scope::Environment(id) => environment.id == *id,
            })
            .map(|environment| Source {
                id: &environment.id,
                name: &environment.name,
                records: state.notification_records_of(&environment.id),
            })
            .collect();
        entry::centre_view(
            &sources,
            scope == Scope::All,
            self.centre.filter.as_ref(),
            now,
        )
    }

    /// The notification centre's card.
    pub(super) fn notification_centre(&self, now: Timestamp, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let view = self.view(state, now);
        let has_environment = state.environment().is_some();
        let header = Self::centre_header(&view, theme, cx);
        let scopes = (state.environments().len() > 1).then(|| self.centre_scopes(state, theme, cx));
        let pause_row = has_environment.then(|| self.centre_pause(now, theme, cx));
        let list = self.centre_list(&view, theme, cx);
        let footer = Self::centre_footer(state, has_environment, theme, cx);
        let card = div()
            .id("notification-centre")
            .occlude()
            .flex()
            .flex_col()
            .w(px(CENTRE_WIDTH))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow(vec![BoxShadow {
                color: colors.shadow_strong,
                offset: point(px(0.), px(6.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(header)
            .children(scopes)
            .children(pause_row)
            .child(list)
            .child(footer);
        // Escape or a press outside closes it.
        Dismissable::new(
            "notification-centre-popup",
            card,
            cx.listener(|this, dismissal: &Dismissal, _, cx| {
                this.menus.dismissed(*dismissal);
                cx.notify();
            }),
        )
        .into_any_element()
    }

    /// `Notifications · 3 unread` (the scope's) and *mark all read* (what
    /// the list shows).
    fn centre_header(view: &CentreView, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .px(px(CENTRE_PADDING))
            .border_b_1()
            .border_color(colors.border_header)
            .child(
                div()
                    .text_size(theme.text.heading)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child("Notifications"),
            )
            .child(
                div()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(entry::summary(view.unread, view.total)),
            )
            .child(div().flex_1())
            .when(view.has_unread(), |header| {
                header.child(
                    Link::new("centre-mark-all-read", "mark all read")
                        .quiet()
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.mark_view_read(cx);
                        })),
                )
            })
            .into_any_element()
    }

    /// Marks what the list shows read: every notification of the scope's
    /// environments, or what the filter left.
    pub(super) fn mark_view_read(&mut self, cx: &mut Context<Self>) {
        let now = Timestamp::now();
        let (environments, ids) = {
            let state = self.state.read(cx);
            let view = self.view(state, now);
            let environments: Vec<String> = match self.scope(state) {
                Some(Scope::All) => state
                    .environments()
                    .iter()
                    .map(|environment| environment.id.clone())
                    .collect(),
                Some(Scope::Environment(id)) => vec![id],
                None => Vec::new(),
            };
            let ids: Vec<(String, String)> = view
                .entries()
                .filter(|entry| entry.unread)
                .map(|entry| (entry.environment.clone(), entry.id.clone()))
                .collect();
            (environments, ids)
        };
        let filtered = self.centre.filter.is_some();
        self.state.update(cx, |state, cx| {
            let mut changed = false;
            for environment in &environments {
                changed |= if filtered {
                    let mine: Vec<String> = ids
                        .iter()
                        .filter(|(id, _)| id == environment)
                        .map(|(_, tag)| tag.clone())
                        .collect();
                    !mine.is_empty() && state.mark_read_in(environment, Some(&mine))
                } else {
                    state.unread_in(environment) > 0 && state.mark_read_in(environment, None)
                };
            }
            if changed {
                cx.notify();
            }
        });
        cx.notify();
    }

    /// The scope tabs (A2): `all`, then every environment; those with
    /// unread notifications in the accent colour; what doesn't fit behind
    /// `···`.
    fn centre_scopes(&self, state: &AppState, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let selected_scope = self.scope(state);
        // `all` shows what the scope picked doesn't: marked for unread
        // notifications in other environments.
        let mut tabs = vec![ScopeTab {
            scope: Scope::All,
            label: "all".to_owned(),
            unread: state.environments().iter().any(|environment| {
                selected_scope != Some(Scope::Environment(environment.id.clone()))
                    && state.unread_in(&environment.id) > 0
            }),
        }];
        tabs.extend(state.environments().iter().map(|environment| ScopeTab {
            scope: Scope::Environment(environment.id.clone()),
            label: tab_name(&environment.name),
            unread: state.unread_in(&environment.id) > 0,
        }));
        let selected = tabs
            .iter()
            .position(|tab| Some(&tab.scope) == selected_scope.as_ref())
            .unwrap_or(0);
        let size = theme.text.body;
        let widths: Vec<f32> = tabs
            .iter()
            .map(|tab| f32::from(sub_tab_width(&tab.label, size)))
            .collect();
        let (shown, hidden) = fit_tabs(
            &widths,
            selected,
            CENTRE_WIDTH - 2. * CENTRE_PADDING - 2.,
            f32::from(SUB_TAB_GAP),
            f32::from(sub_tab_width(MORE, size)),
        );
        let shown_scopes: Vec<Scope> = shown
            .iter()
            .map(|index| tabs[*index].scope.clone())
            .collect();
        let mut row = SubTabs::new("centre-scopes")
            .selected(
                shown
                    .iter()
                    .position(|index| *index == selected)
                    .unwrap_or(0),
            )
            .on_select({
                let entity = cx.entity();
                move |index, _, cx| {
                    if let Some(scope) = shown_scopes.get(index) {
                        entity.update(cx, |this, cx| {
                            this.pick_scope(scope.clone());
                            cx.notify();
                        });
                    }
                }
            });
        for index in &shown {
            let tab = &tabs[*index];
            row = row.marked_tab(tab.label.clone(), tab.unread);
        }
        if !hidden.is_empty() {
            let items: Vec<(Scope, String, bool)> = hidden
                .iter()
                .map(|index| {
                    let tab = &tabs[*index];
                    let name = match &tab.scope {
                        Scope::Environment(id) => state.environment_by_id(id).map_or_else(
                            || tab.label.clone(),
                            |environment| environment.name.clone(),
                        ),
                        Scope::All => tab.label.clone(),
                    };
                    (tab.scope.clone(), name, tab.unread)
                })
                .collect();
            row = row.trailing(self.scope_more(items, theme, cx));
        }
        div()
            .flex_none()
            .px(px(CENTRE_PADDING))
            .pt(px(10.))
            .child(row)
            .into_any_element()
    }

    /// The `···` after the scope tabs that fit, with the others in a menu;
    /// the accent colour while one of them has unread notifications.
    fn scope_more(
        &self,
        items: Vec<(Scope, String, bool)>,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let marked = items.iter().any(|(_, _, unread)| *unread);
        let open = self.centre.overflow_open;
        let count = items.len();
        div()
            .id("centre-scope-more")
            .relative()
            .cursor_pointer()
            .text_color(if marked {
                colors.accent
            } else if open {
                colors.text
            } else {
                colors.text_muted
            })
            .hover(move |style| {
                style.text_color(if marked {
                    colors.accent_hover
                } else {
                    colors.text
                })
            })
            .child(MORE)
            .when(!open, |trigger| {
                trigger.tooltip(Tooltip::text(format!("{count} more environments")))
            })
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.centre.overflow_open = !this.centre.overflow_open;
                cx.notify();
            }))
            .when(open, |trigger| {
                trigger.child(
                    Popover::new(Self::scope_overflow(items, cx))
                        .align_right()
                        .gap(px(10.)),
                )
            })
            .into_any_element()
    }

    /// The scopes that didn't fit, as a menu.
    fn scope_overflow(items: Vec<(Scope, String, bool)>, cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("centre-scope-overflow").min_width(px(160.));
        for (index, (scope, name, unread)) in items.into_iter().enumerate() {
            menu = menu.item(
                MenuItem::new(("centre-scope-hidden", index), name)
                    .highlighted(unread)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.pick_scope(scope.clone());
                        cx.notify();
                    })),
            );
        }
        menu.on_dismiss(cx.listener(|this, _: &Dismissal, _, cx| {
            this.centre.overflow_open = false;
            cx.notify();
        }))
    }

    /// Shows `scope`'s notifications, all of them.
    fn pick_scope(&mut self, scope: Scope) {
        self.centre.scope = Some(scope);
        self.centre.filter = None;
        self.centre.overflow_open = false;
    }

    /// Pausing every environment (30 minutes, an hour, until 08:00), or
    /// resuming (A5: the pause is global by default); while the
    /// environment on screen is muted on its own, until when, and
    /// *unmute*. One line of a chip's height in every case, so the centre
    /// (anchored above the footer) never moves when it changes.
    fn centre_pause(&self, now: Timestamp, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let state = self.state.read(cx);
        let environments = state.environments().len();
        let paused = state.paused_until().filter(|until| *until > now);
        let muted = state.environment().and_then(|environment| {
            state
                .environment_paused_until(&environment.id, now)
                .map(|until| (environment.id.clone(), environment.name.clone(), until))
        });
        let row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(CENTRE_PADDING))
            .py(px(8.))
            .border_b_1()
            .border_color(colors.border_row)
            .text_size(theme.text.small);
        if let Some(until) = paused {
            return row
                .child(
                    div()
                        .flex_1()
                        .line_height(px(CHIP_HEIGHT))
                        .text_color(theme.states.warning)
                        .child(format!(
                            "{}: shown silently",
                            paused_text(environments, until, now)
                        )),
                )
                .child(
                    Link::new("centre-resume", "resume")
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                state.pause_notifications(None);
                                cx.notify();
                            });
                        })),
                )
                .into_any_element();
        }
        if let Some((id, name, until)) = muted {
            return row
                .child(
                    div()
                        .flex_1()
                        .line_height(px(CHIP_HEIGHT))
                        .min_w_0()
                        .truncate()
                        .text_color(theme.states.warning)
                        .child(format!(
                            "{name} muted until {}: shown silently",
                            when(until, now)
                        )),
                )
                .child(
                    Link::new("centre-unmute", "unmute")
                        .text_size(theme.text.small)
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                if state.pause_environment(&id, None) {
                                    cx.notify();
                                }
                            });
                        })),
                )
                .into_any_element();
        }
        row.child(
            div()
                .flex_1()
                .line_height(px(CHIP_HEIGHT))
                .text_color(colors.text_faint)
                .child(pause_label(environments)),
        )
        .children(PauseChoice::ALL.map(|choice| {
            Chip::new(
                SharedString::from(format!("centre-pause-{choice:?}")),
                choice.short_label(),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.update(cx, |state, cx| {
                    state.pause_notifications(Some(choice.until(Timestamp::now())));
                    cx.notify();
                });
            }))
        }))
        .into_any_element()
    }

    /// The sections and their entries, newest first, or why there are
    /// none.
    fn centre_list(&self, view: &CentreView, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        if view.sections.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(CENTRE_PADDING))
                .py(px(18.))
                .text_size(theme.text.small)
                .child(
                    div()
                        .text_color(colors.text_muted)
                        .child("No notifications yet."),
                )
                .child(div().text_color(colors.text_faint).child(
                    "They appear here as they happen, silent ones too: quiet hours, pauses \
                     and storms are recorded without a system notification.",
                ))
                .into_any_element();
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut index = 0;
        for (number, section) in view.sections.iter().enumerate() {
            rows.push(
                div()
                    .flex_none()
                    .px(px(CENTRE_PADDING))
                    .pt(px(if number == 0 { 6. } else { 10. }))
                    .pb(px(4.))
                    .text_size(theme.text.caption)
                    .text_color(colors.text_faint)
                    .child(section.when.title())
                    .into_any_element(),
            );
            for item in &section.items {
                match item {
                    CentreItem::Entry(entry) => {
                        rows.push(self.centre_row(index, entry, None, false, theme, cx));
                        index += 1;
                    }
                    CentreItem::Storm(group) => {
                        let expanded = self.centre.expanded.contains(&group.key);
                        rows.push(self.centre_row(
                            index,
                            &group.header,
                            Some(group),
                            false,
                            theme,
                            cx,
                        ));
                        index += 1;
                        if expanded {
                            for member in &group.members {
                                rows.push(self.centre_row(index, member, None, true, theme, cx));
                                index += 1;
                            }
                        }
                    }
                }
            }
        }
        div()
            .id("centre-list")
            .flex()
            .flex_col()
            .max_h(px(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .pb(px(4.))
            .children(rows)
            .into_any_element()
    }

    /// Where the history stays and for how long, and the settings.
    fn centre_footer(
        state: &AppState,
        has_environment: bool,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let retention = state.config().general.event_log_retention_hours;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(36.))
            .px(px(CENTRE_PADDING))
            .border_t_1()
            .border_color(colors.border_header)
            .text_size(theme.text.small)
            .text_color(colors.text_faint)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(entry::kept_text(retention)),
            )
            .when(has_environment, |footer| {
                footer.child(
                    Link::new("centre-settings", "notification settings…")
                        .quiet()
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.menus.close();
                            cx.emit(SidebarEvent::OpenSettings(SettingsTab::Notifications));
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    /// One entry: time, tone dot, title, first line, where and whether
    /// silent. A storm's summary (`storm`) shows how many it held back and
    /// expands; a storm's notification (`member`) is indented under it.
    fn centre_row(
        &self,
        index: usize,
        entry: &CentreEntry,
        storm: Option<&StormGroup>,
        member: bool,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let tone = tone_color(entry.tone, theme);
        let unread = storm.map_or(entry.unread, StormGroup::unread);
        let silent = entry.is_silent();
        // Unread is a brighter title; silent ones are a step dimmer.
        let title_color = match (unread, silent) {
            (true, false) => colors.text_strong,
            (true, true) => colors.text_secondary,
            (false, false) => colors.text_muted,
            (false, true) => colors.text_faint,
        };
        let expanded = storm.is_some_and(|group| self.centre.expanded.contains(&group.key));
        let detail = self.detail_line(index, entry, storm, theme, cx);
        let environment = entry.environment.clone();
        let id = entry.id.clone();
        let object = entry.object.clone();
        let storm_key = storm.map(|group| group.key.clone());
        let row = div()
            .id(SharedString::from(format!("centre-entry-{index}")))
            .flex()
            .items_start()
            .gap(px(10.))
            .pl(px(if member {
                CENTRE_PADDING + f32::from(theme.metrics.row_indent)
            } else {
                CENTRE_PADDING
            }))
            .pr(px(CENTRE_PADDING))
            .py(px(7.))
            .cursor_pointer()
            .hover(|style| style.bg(colors.row_hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if let Some(key) = &storm_key {
                    if !this.centre.expanded.remove(key) {
                        this.centre.expanded.insert(key.clone());
                    }
                    cx.notify();
                    return;
                }
                this.open_entry(&environment, &id, object.as_ref(), cx);
            }))
            .child(
                div()
                    .w(px(44.))
                    .flex_none()
                    .pt(px(1.))
                    .text_size(theme.text.hint)
                    .text_color(colors.text_faint)
                    .child(entry.time.clone()),
            )
            .child(
                div()
                    .pt(px(5.))
                    .flex_none()
                    .child(StateDot::with_color(tone).size(px(7.)).hollow(silent)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        div()
                            .truncate()
                            .text_size(theme.text.body)
                            .text_color(title_color)
                            .child(entry.title.clone()),
                    )
                    .when(!entry.body.is_empty(), |column| {
                        column.child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_faint)
                                .child(entry.body.clone()),
                        )
                    })
                    .children(detail),
            )
            .when(storm.is_some(), |row| {
                row.child(
                    div().pt(px(3.)).flex_none().child(
                        Icon::new(if expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size(px(12.))
                        .color(colors.text_faint),
                    ),
                )
            });
        match &entry.silence {
            Some(silence) if storm.is_none() => row
                .tooltip(Tooltip::text(entry::silent_hint(silence)))
                .into_any_element(),
            _ => row.into_any_element(),
        }
    }

    /// An entry's last line: its label (a click filters to its place; the
    /// active filter in the accent colour), why it was silent, and for a
    /// storm how many it held back.
    fn detail_line(
        &self,
        index: usize,
        entry: &CentreEntry,
        storm: Option<&StormGroup>,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let colors = theme.colors;
        let mut extra: Vec<String> = Vec::new();
        if let Some(silence) = &entry.silence {
            extra.push(silence.clone());
        }
        if let Some(group) = storm {
            extra.push(format!("{} held back", group.members.len()));
        }
        if entry.label.is_none() && extra.is_empty() {
            return None;
        }
        let label = entry.label.as_ref().map(|label| {
            let active = self.centre.filter.as_ref() == Some(&label.place);
            let place = label.place.clone();
            let link = Link::new(
                SharedString::from(format!("centre-label-{index}")),
                label.text.clone(),
            )
            .text_size(theme.text.hint)
            .truncate()
            .tooltip(Tooltip::new(if active {
                "Show every notification again"
            } else {
                "Show only the notifications from here"
            }))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.centre.filter = if this.centre.filter.as_ref() == Some(&place) {
                    None
                } else {
                    Some(place.clone())
                };
                cx.notify();
            }));
            if active { link } else { link.quiet() }
        });
        let rest = (!extra.is_empty()).then(|| {
            let text = extra.join(" · ");
            if label.is_some() {
                format!("· {text}")
            } else {
                text
            }
        });
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(5.))
                .min_w_0()
                .text_size(theme.text.hint)
                .text_color(colors.text_faint)
                .children(label)
                .when_some(rest, |line, rest| {
                    line.child(div().flex_none().whitespace_nowrap().child(rest))
                })
                .into_any_element(),
        )
    }

    /// Opens an entry: marks it read and shows its object, in its own
    /// environment (switching there first).
    fn open_entry(
        &mut self,
        environment: &str,
        id: &str,
        object: Option<&ObjectKey>,
        cx: &mut Context<Self>,
    ) {
        self.state.update(cx, |state, cx| {
            if state.mark_notification_read_in(environment, id) {
                cx.notify();
            }
        });
        if let Some(object) = object {
            self.menus.close();
            cx.emit(SidebarEvent::OpenObjectIn {
                environment: environment.to_owned(),
                object: object.clone(),
            });
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tabs_that_fit_show_and_the_rest_go_behind_more() {
        // Everything fits.
        assert_eq!(
            fit_tabs(&[20., 90., 50.], 1, 300., 22., 22.),
            (vec![0, 1, 2], vec![])
        );
        // Too many: `all` stays, then as many as fit beside `···`.
        let widths = [20., 90., 90., 90., 90.];
        assert_eq!(
            fit_tabs(&widths, 1, 300., 22., 22.),
            (vec![0, 1, 2], vec![3, 4])
        );
        // A hidden selected tab takes the last place.
        assert_eq!(
            fit_tabs(&widths, 4, 300., 22., 22.),
            (vec![0, 1, 4], vec![2, 3])
        );
        // `all` selected, nothing else fits.
        assert_eq!(
            fit_tabs(&[20., 400.], 0, 100., 22., 22.),
            (vec![0], vec![1])
        );
    }

    #[test]
    fn long_names_are_cut_for_tabs() {
        assert_eq!(tab_name("staging"), "staging");
        let name = tab_name("a-very-long-environment-name");
        assert_eq!(name.chars().count(), TAB_NAME_CHARS);
        assert!(name.ends_with('…'));
    }
}
