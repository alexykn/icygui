//! The notification centre (NOTE-05, A2): opened from the footer's clock
//! icon, above it.
//!
//! - The heading counts what is unread in the scope; *mark all read* marks
//!   what the list shows (the scope, or what a label filter left).
//! - With several environments a row of chips at the bottom picks the
//!   scope: one environment (the one on screen when the centre opens) or
//!   *all*; the one picked filled with the selected-row background, like
//!   the lists' selection. A scope with unread notifications shows its
//!   name in the accent colour (colour only: nothing moves); chips that
//!   don't fit go into a `···` menu, so the row never wraps.
//! - The pause row pauses every environment (A5: the pause is global), or
//!   shows that the environment of the scope is muted on its own, with
//!   *unmute* (every environment can be muted from the switcher).
//! - The list, newest first, under `now`, `last hour`, `earlier today`,
//!   `yesterday`, `older`: unread entries have a bright title; silent ones
//!   a hollow dot, a dimmer title and why (`silent · storm`). A storm's
//!   notifications collapse into its summary (a click shows them). A
//!   click on an entry opens its object (switching to its environment)
//!   and marks it read; a click on its label (`overview`) shows only that
//!   place's, again shows everything.
//! - A gear at the bottom right opens the notification settings.
//!
//! The card keeps its size and place while it is open: the list has a
//! fixed height (440 px, less in a short window, so the card always fits
//! above the footer), whatever the scope, the filter, the storms
//! expanded or the notifications arriving, so the tabs and chips never
//! move under the pointer. While the pointer is over the list, arriving
//! notifications wait (the heading counts them) until it leaves, so the
//! entries don't move under it either.

use std::collections::HashSet;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
};
use gpui::{App, BoxShadow, point};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::Tone;
use ic_ui_kit::{
    ActiveTheme as _, CHIP_HEIGHT, Chip, Dismissable, Dismissal, Icon, IconButton, IconName, Link,
    Menu, MenuItem, Popover, StateDot, Theme, Tooltip, chip_width, px,
};

use super::{Sidebar, SidebarEvent, SidebarMenu};
use crate::app_state::AppState;
use crate::notifications::entry::{
    self, CentreEntry, CentreItem, CentreView, Place, Scope, Source, StormGroup,
};
use crate::notifications::{PauseChoice, pause_label, paused_text, when};
use crate::settings::SettingsPage;

/// The centre's width.
const CENTRE_WIDTH: f32 = 420.;
/// The centre's inner padding, left and right.
const CENTRE_PADDING: f32 = 14.;
/// The list's height (it scrolls beyond), in a window tall enough.
const LIST_MAX_HEIGHT: f32 = 440.;
/// The least the list gets in a short window.
const LIST_MIN_HEIGHT: f32 = 120.;
/// Under the window's height: the footer, the gap above it and the margin
/// the card keeps from the window's top.
const OUTSIDE: f32 = 50.;
/// The card's parts around the list: the heading, the pause row, the
/// bottom bar and the border.
const CHROME: f32 = 40. + 39. + BAR_HEIGHT + 2.;
/// The bottom bar's height: the scope chips and the settings gear.
const BAR_HEIGHT: f32 = 36.;
/// The space between two scope chips (as between the pause chips).
const CHIP_GAP: f32 = 6.;

/// The list's height in a window `viewport` high: fixed while the centre
/// is open, so nothing in the card moves with what the list shows; less
/// in a short window, so the card fits above the footer.
pub(super) fn list_height(viewport: f32) -> f32 {
    (viewport - OUTSIDE - CHROME).clamp(LIST_MIN_HEIGHT, LIST_MAX_HEIGHT)
}
/// The longest environment name a scope chip shows in full.
const TAB_NAME_CHARS: usize = 20;
/// The scope overflow's chip.
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
    /// Whether the scope chips' `···` menu is open.
    overflow_open: bool,
    /// The list's scrolling (and where it is drawn).
    list: gpui::ScrollHandle,
    /// While the pointer is over the list: the notifications there were
    /// when it came (by intent id). Newer ones wait until it leaves, so
    /// the entries never move under the pointer (the heading counts them
    /// at once).
    held: Option<HashSet<String>>,
}

/// The colour of a notification's tone.
pub(crate) fn tone_color(tone: Tone, theme: &Theme) -> gpui::Hsla {
    match tone {
        Tone::Critical => theme.states.fill.critical,
        Tone::Warning => theme.states.fill.warning,
        Tone::Unknown => theme.states.fill.unknown,
        Tone::Recovery => theme.states.fill.ok,
        Tone::Info => theme.colors.accent,
    }
}

/// What the centre's pause row shows.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum PauseLine {
    /// Every environment is paused until then: *resume*.
    Paused(Timestamp),
    /// The scope's environment is muted on its own until then: *unmute*.
    Muted {
        /// The environment's id.
        id: String,
        /// Its name.
        name: String,
        /// Until when.
        until: Timestamp,
    },
    /// The pause of every environment (A5: global by default).
    Offer,
}

/// The pause row for `scope` at `now`: the pause of every environment
/// first; else the scope's environment's own mute (`all` and the other
/// environments offer the pause of every environment).
pub(super) fn pause_line(state: &AppState, scope: Option<&Scope>, now: Timestamp) -> PauseLine {
    if let Some(until) = state.paused_until().filter(|until| *until > now) {
        return PauseLine::Paused(until);
    }
    let Some(Scope::Environment(id)) = scope else {
        return PauseLine::Offer;
    };
    state
        .environment_by_id(id)
        .and_then(|environment| {
            state
                .environment_paused_until(id, now)
                .map(|until| PauseLine::Muted {
                    id: id.clone(),
                    name: environment.name.clone(),
                    until,
                })
        })
        .unwrap_or(PauseLine::Offer)
}

/// A scope chip: what it selects and its label.
struct ScopeTab {
    scope: Scope,
    label: String,
    unread: bool,
}

/// Which of `widths` (chips in order, the first always shown) fit in
/// `available` with `gap` between them, keeping `selected` among them; the
/// rest go behind a trigger `more` wide. Returns the shown and the hidden
/// chips' indices, each in order.
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

/// `name` short enough for a chip (`…` at the end: cut short).
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
    pub(crate) fn centre_scope(&self, cx: &App) -> Option<Scope> {
        self.scope(self.state.read(cx))
    }

    /// The centre's list as shown now.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn centre_view(&self, cx: &App) -> CentreView {
        self.view(self.state.read(cx), Timestamp::now())
    }

    /// Where the centre's list is drawn: it stays put while the centre is
    /// open, whatever it shows.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn centre_list_bounds(&self) -> gpui::Bounds<gpui::Pixels> {
        self.centre.list.bounds()
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
        let Some(held) = &self.centre.held else {
            return entry::centre_view(
                &sources,
                scope == Scope::All,
                self.centre.filter.as_ref(),
                now,
            );
        };
        // The pointer is over the list: what arrived since waits, the
        // heading counts it already.
        let (total, unread) = sources
            .iter()
            .flat_map(|source| &source.records)
            .fold((0, 0), |(total, unread), record| {
                (total + 1, unread + usize::from(!record.read))
            });
        let shown: Vec<Source<'_>> = sources
            .into_iter()
            .map(|source| Source {
                records: source
                    .records
                    .into_iter()
                    .filter(|record| held.contains(&record.intent.id))
                    .collect(),
                ..source
            })
            .collect();
        let mut view = entry::centre_view(
            &shown,
            scope == Scope::All,
            self.centre.filter.as_ref(),
            now,
        );
        view.total = total;
        view.unread = unread;
        view
    }

    /// The pointer came over the centre's list (`hovered`) or left it:
    /// while it is there, notifications arriving wait, so nothing moves
    /// under it (NOTE-05); leaving lists them.
    fn hold_centre(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if !hovered {
            if self.centre.held.take().is_some() {
                cx.notify();
            }
            return;
        }
        let state = self.state.read(cx);
        let listed = state
            .environments()
            .iter()
            .flat_map(|environment| state.notification_records_of(&environment.id))
            .map(|record| record.intent.id.clone())
            .collect();
        self.centre.held = Some(listed);
    }

    /// Whether arrivals wait because the pointer is over the list.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn centre_holds(&self) -> bool {
        self.centre.held.is_some()
    }

    /// The notification centre's card.
    pub(super) fn notification_centre(&self, now: Timestamp, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let view = self.view(state, now);
        let has_environment = state.environment().is_some();
        let header = Self::centre_header(&view, theme, cx);
        let pause_row = has_environment.then(|| self.centre_pause(now, theme, cx));
        // The window's height in the design's pixels: the card's parts
        // scale with the interface size, the window doesn't.
        let height = list_height(f32::from(self.viewport) / ic_ui_kit::scale());
        let list = self.centre_list(&view, height, theme, cx);
        let bar = self.centre_bar(state, has_environment, theme, cx);
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
            .children(pause_row)
            .child(list)
            .child(bar);
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

    /// The bottom bar: with several environments the scope chips (A2):
    /// `all`, then every environment; the one picked filled with the
    /// selected-row background; those with unread notifications in the
    /// accent colour; what doesn't fit behind `···`. At the right a gear
    /// for the notification settings. One left and right edge with the
    /// heading, the pause row and the list; the bar's height never
    /// changes.
    fn centre_bar(
        &self,
        state: &AppState,
        has_environment: bool,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let settings = has_environment.then(|| {
            IconButton::new("centre-settings", IconName::Settings)
                .tooltip(Tooltip::new("Notification settings"))
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    cx.emit(SidebarEvent::OpenSettings(SettingsPage::Notifications));
                    cx.notify();
                }))
        });
        let scopes = (state.environments().len() > 1).then(|| self.centre_scopes(state, theme, cx));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(BAR_HEIGHT))
            .px(px(CENTRE_PADDING))
            .border_t_1()
            .border_color(colors.border_header)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(px(CHIP_GAP))
                    .children(scopes),
            )
            .children(settings)
            .into_any_element()
    }

    /// The scope chips: `all`, then every environment, those that fit the
    /// bar beside the gear, then `···` for the rest.
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
        let size = theme.text.label;
        let widths: Vec<f32> = tabs
            .iter()
            .map(|tab| f32::from(chip_width(&tab.label, size)))
            .collect();
        // In window pixels: the chips' widths follow the interface size.
        let scale = ic_ui_kit::scale();
        let gear = f32::from(theme.metrics.icon_button) + 8. * scale;
        let (shown, hidden) = fit_tabs(
            &widths,
            selected,
            (CENTRE_WIDTH - 2. * CENTRE_PADDING) * scale - 2. - gear,
            CHIP_GAP * scale,
            f32::from(chip_width(MORE, size)),
        );
        let mut chips: Vec<AnyElement> = shown
            .iter()
            .map(|index| {
                let tab = &tabs[*index];
                let scope = tab.scope.clone();
                Chip::new(("centre-scope", *index), tab.label.clone())
                    .filled(*index == selected)
                    .marked(tab.unread)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.pick_scope(scope.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
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
            chips.push(self.scope_more(items, cx));
        }
        div()
            .flex()
            .items_center()
            .gap(px(CHIP_GAP))
            .children(chips)
            .into_any_element()
    }

    /// The `···` chip after the scope chips that fit, with the others in a
    /// menu above it; the accent colour while one of them has unread
    /// notifications.
    fn scope_more(&self, items: Vec<(Scope, String, bool)>, cx: &Context<Self>) -> AnyElement {
        let marked = items.iter().any(|(_, _, unread)| *unread);
        let open = self.centre.overflow_open;
        let count = items.len();
        div()
            .id("centre-scope-more")
            .relative()
            .flex_none()
            .when(!open, |trigger| {
                trigger.tooltip(Tooltip::text(format!("{count} more environments")))
            })
            .child(
                Chip::new("centre-scope-more-chip", MORE)
                    .marked(marked)
                    .filled(open)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.centre.overflow_open = !this.centre.overflow_open;
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                trigger.child(
                    Popover::new(Self::scope_overflow(items, cx))
                        .above()
                        .gap(px(6.)),
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
    /// environment of the scope is muted on its own, until when, and
    /// *unmute* (*all* and the other environments' scopes still offer the
    /// pause of every environment). One line of a chip's height in every
    /// case, so nothing moves when it changes.
    fn centre_pause(&self, now: Timestamp, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let state = self.state.read(cx);
        let environments = state.environments().len();
        let (paused, muted) = match pause_line(state, self.scope(state).as_ref(), now) {
            PauseLine::Paused(until) => (Some(until), None),
            PauseLine::Muted { id, name, until } => (None, Some((id, name, until))),
            PauseLine::Offer => (None, None),
        };
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
                        .text_color(theme.states.text.warning)
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
            let text = format!("{name} muted until {}: shown silently", when(until, now));
            return row
                .child(
                    div()
                        .id("centre-muted")
                        .flex_1()
                        .line_height(px(CHIP_HEIGHT))
                        .min_w_0()
                        .truncate()
                        .text_color(theme.states.text.warning)
                        .tooltip(Tooltip::text(text.clone()))
                        .child(text),
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
    /// none, `height` high (it scrolls beyond).
    fn centre_list(
        &self,
        view: &CentreView,
        height: f32,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        if view.sections.is_empty() {
            return div()
                .id("centre-list")
                .track_scroll(&self.centre.list)
                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                    this.hold_centre(*hovered, cx);
                }))
                .flex()
                .flex_none()
                .flex_col()
                .h(px(height))
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
            .track_scroll(&self.centre.list)
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                this.hold_centre(*hovered, cx);
            }))
            .flex()
            .flex_none()
            .flex_col()
            .h(px(height))
            .overflow_y_scroll()
            .pb(px(4.))
            .children(rows)
            .into_any_element()
    }

    /// Whether the entry's object counts as handled now (acknowledged, or
    /// a downtime in effect), in the entry's environment: its dot is hollow.
    fn counts_as_handled(&self, entry: &CentreEntry, cx: &App) -> bool {
        entry.object.as_ref().is_some_and(|object| {
            let state = self.state.read(cx);
            let snapshot = state
                .snapshot_of(&entry.environment)
                .unwrap_or_else(|| state.snapshot());
            crate::operate::dialog::object_mark(snapshot, object).is_some_and(|mark| mark.hollow)
        })
    }

    /// One entry: time, tone dot, title, first line, where and whether
    /// silent. The dot is hollow when its object counts as handled now
    /// (acknowledged, or a downtime in effect: hollow = handled, as in
    /// every list). A storm's summary (`storm`) shows how many it held
    /// back and expands; a storm's notification (`member`) is indented
    /// under it.
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
        // Compact rows drop the output line, as in the dashboard list.
        let compact = theme.density == ic_ui_kit::Density::Compact;
        let tone = tone_color(entry.tone, theme);
        let unread = storm.map_or(entry.unread, StormGroup::unread);
        let handled = storm.is_none() && self.counts_as_handled(entry, cx);
        // Unread is a brighter title; silent ones are a step dimmer.
        let title_color = match (unread, entry.is_silent()) {
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
            .pl(if member {
                px(CENTRE_PADDING) + theme.metrics.row_indent
            } else {
                px(CENTRE_PADDING)
            })
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
                    .child(StateDot::with_color(tone).size(px(7.)).hollow(handled)),
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
                    .when(!compact && !entry.body.is_empty(), |column| {
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
        // A summary says how many it held back; a storm still going on
        // says so in its title already.
        if let Some(group) = storm.filter(|group| !group.ongoing) {
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
    fn the_pause_row_is_the_scopes() {
        use crate::live::demo;
        let now = Timestamp::now();
        let later = Timestamp::from_unix_seconds(now.as_unix_seconds() + 3600.);
        let mut state = AppState::demo(demo::config(), now);
        let prod = Scope::Environment(demo::ENVIRONMENT_ID.to_owned());
        let lab = Scope::Environment(demo::LAB_ID.to_owned());
        assert_eq!(pause_line(&state, Some(&prod), now), PauseLine::Offer);
        // prod-cluster muted: its own scope says so; lab's and `all` still
        // offer to pause every environment.
        assert!(state.pause_environment(demo::ENVIRONMENT_ID, Some(later)));
        assert_eq!(
            pause_line(&state, Some(&prod), now),
            PauseLine::Muted {
                id: demo::ENVIRONMENT_ID.to_owned(),
                name: "prod-cluster".to_owned(),
                until: later,
            }
        );
        assert_eq!(pause_line(&state, Some(&lab), now), PauseLine::Offer);
        assert_eq!(pause_line(&state, Some(&Scope::All), now), PauseLine::Offer);
        // Every environment paused: said in every scope.
        state.pause_notifications(Some(later));
        for scope in [&prod, &lab, &Scope::All] {
            assert_eq!(
                pause_line(&state, Some(scope), now),
                PauseLine::Paused(later)
            );
        }
    }

    #[test]
    fn the_list_keeps_its_height_and_fits_short_windows() {
        // The default window: the full list.
        assert!((list_height(900.) - LIST_MAX_HEIGHT).abs() < f32::EPSILON);
        // The smallest window (560 px): the whole card stays above the
        // footer, the list gets what is left.
        let short = list_height(560.);
        assert!(
            (LIST_MIN_HEIGHT..LIST_MAX_HEIGHT).contains(&short),
            "{short}"
        );
        assert!(short + CHROME + OUTSIDE <= 560.);
        // Never less than a few entries.
        assert!((list_height(200.) - LIST_MIN_HEIGHT).abs() < f32::EPSILON);
    }

    #[test]
    fn long_names_are_cut_for_tabs() {
        assert_eq!(tab_name("staging"), "staging");
        let name = tab_name("a-very-long-environment-name");
        assert_eq!(name.chars().count(), TAB_NAME_CHARS);
        assert!(name.ends_with('…'));
    }
}
