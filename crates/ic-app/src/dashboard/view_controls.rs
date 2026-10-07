//! The controls in a view's header on the dashboard page (topic 14,
//! round 5; README, *View controls*): one set per view, the same stacked
//! (compact, in the view's 36px header) and on a one-view page (roomy, in
//! the page header).
//!
//! - **Counts** (lists, grids, tiles): three fixed slots, critical,
//!   warning, unknown (a list of hosts: down, none, unreachable), each a
//!   dot and room for three digits; a state at zero keeps its slot, empty,
//!   so a count going from 9 to 427 moves nothing. A list's counts are its
//!   state chips: a click shows only that state ([`View::state`]), a click
//!   on the picked one shows them all again.
//! - **Chips** (handling, downtimes): as on the view's own page, their
//!   words dropping where the header is narrow (the tooltip names them).
//! - **timeline | list** (downtimes): the compact segmented control.
//! - **Sort** (handling, downtimes): the view's own sort menu.
//! - **Rows**: what picking a density does ([`crate::controls`]).

use std::rc::Rc;

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, MouseButton,
    ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _,
};
use ic_config::{ObjectKind, StateChip, View};
use ic_core::snapshot::Summary;
use ic_model::{CheckableState, HostState, ServiceState};
use ic_rules::DashboardRef;
use ic_ui_kit::{ActiveTheme as _, Menu, MenuItem, Segmented, StateDot, Theme, Tooltip, px};

use super::DashboardView;
use super::header::{HeaderMenu, small_chars, sort_word};
use super::page::ThreadPage;
use crate::controls::PickDensity;
use crate::lists::model::{Chip, ListKind, Mode, Options};

/// Digits a count slot holds (`427`).
const COUNT_DIGITS: usize = 3;
/// Between two count slots.
const SLOT_GAP: f32 = 6.;
/// A count slot's dot.
const SLOT_DOT: f32 = 7.;
/// Between a slot's dot and its count.
const SLOT_DOT_GAP: f32 = 5.;
/// A slot's padding either side (its picked look's border inside it).
const SLOT_PADDING: f32 = 4.;
/// Between the chips of a handling or downtimes view.
const CHIP_GAP: f32 = 6.;

/// One of a header's three count slots: its colour, its count, what it
/// counts, and the state chip a click picks (lists only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CountSlot {
    pub(crate) state: CheckableState,
    pub(crate) count: u32,
    pub(crate) word: &'static str,
    pub(crate) chip: Option<StateChip>,
}

/// The three slots of a header's counts, red, yellow and purple (`None`:
/// a slot that never shows anything, as a list of hosts' yellow one). A
/// list's slots are its state chips; a list without problems shows what it
/// lists (OK, pending) instead, not clickable. A grid colours a host by its
/// services too, so its counts mix both kinds.
pub(crate) fn count_slots(summary: &Summary, view: &View) -> [Option<CountSlot>; 3] {
    let slot = |state, count, word, chip| {
        Some(CountSlot {
            state,
            count,
            word,
            chip,
        })
    };
    if !view.is_list() {
        return [
            slot(
                CheckableState::Service(ServiceState::Critical),
                summary.critical + summary.down,
                "critical",
                None,
            ),
            slot(
                CheckableState::Service(ServiceState::Warning),
                summary.warning,
                "warning",
                None,
            ),
            slot(
                CheckableState::Service(ServiceState::Unknown),
                summary.unknown + summary.unreachable,
                "unknown",
                None,
            ),
        ];
    }
    let problems = match view.object_kind {
        ObjectKind::Services => summary.critical + summary.warning + summary.unknown,
        ObjectKind::Hosts => summary.down + summary.unreachable,
    };
    if problems == 0 && view.state.is_none() {
        // Nothing to filter by: what the list lists, in the slots' order.
        let mut quiet = super::header::summary_items(summary, view.object_kind)
            .into_iter()
            .map(|(state, count, word)| slot(state, count, word, None));
        return [
            quiet.next().flatten(),
            quiet.next().flatten(),
            quiet.next().flatten(),
        ];
    }
    match view.object_kind {
        ObjectKind::Services => [
            slot(
                CheckableState::Service(ServiceState::Critical),
                summary.critical,
                "critical",
                Some(StateChip::Critical),
            ),
            slot(
                CheckableState::Service(ServiceState::Warning),
                summary.warning,
                "warning",
                Some(StateChip::Warning),
            ),
            slot(
                CheckableState::Service(ServiceState::Unknown),
                summary.unknown,
                "unknown",
                Some(StateChip::Unknown),
            ),
        ],
        ObjectKind::Hosts => [
            slot(
                CheckableState::Host(HostState::Down),
                summary.down,
                "down",
                Some(StateChip::Down),
            ),
            None,
            slot(
                CheckableState::Host(HostState::Unreachable),
                summary.unreachable,
                "unreachable",
                Some(StateChip::Unreachable),
            ),
        ],
    }
}

/// A count slot's width: its padding, dot, gap and three digits.
fn slot_width(theme: &Theme) -> Pixels {
    px(2. * SLOT_PADDING + 2. + SLOT_DOT + SLOT_DOT_GAP) + small_chars(theme, COUNT_DIGITS)
}

/// The counts' width: three slots and the gaps between them.
pub(crate) fn counts_width(theme: &Theme) -> Pixels {
    slot_width(theme) * 3. + px(2. * SLOT_GAP)
}

/// The chips' width in a view header (`narrow`: marks and counts only).
pub(crate) fn chips_width(kind: ListKind, narrow: bool, theme: &Theme) -> Pixels {
    let chips = kind.chips();
    #[expect(clippy::cast_precision_loss, reason = "a handful of chips")]
    let gaps = px(CHIP_GAP) * chips.len().saturating_sub(1) as f32;
    chips
        .iter()
        .map(|chip| {
            let widest = match (chip, narrow) {
                (Chip::All, _) => chip.label(kind).to_owned(),
                (_, true) => "999".to_owned(),
                (_, false) => format!("999 {}", chip.label(kind)),
            };
            ic_ui_kit::chip_width(&widest, theme.text.label)
                + if *chip == Chip::All {
                    px(0.)
                } else {
                    px(11. + 6.)
                }
        })
        .sum::<Pixels>()
        + gaps
}

/// The compact `timeline | list` switch's width.
pub(crate) fn mode_width(theme: &Theme) -> Pixels {
    small_label_chars(theme, "timeline".len() + "list".len()) + px(4. * 8. + 3.)
}

fn small_label_chars(theme: &Theme, chars: usize) -> Pixels {
    #[expect(clippy::cast_precision_loss, reason = "a short label")]
    let chars = chars as f32;
    (theme.text.label * (chars * ic_ui_kit::CHAR_WIDTH)).ceil()
}

impl DashboardView {
    /// What picking a density on view `view_id` does: saves it with the
    /// view (the events page: with the environment).
    pub(super) fn pick_density(
        reference: &DashboardRef,
        view_id: &str,
        cx: &Context<Self>,
    ) -> PickDensity {
        let this = cx.entity().downgrade();
        let reference = reference.clone();
        let view_id = view_id.to_owned();
        Rc::new(move |density, _, cx| {
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    this.menus.close();
                    this.change_view(&reference, &view_id, move |view| view.density = density, cx);
                    cx.notify();
                });
            }
        })
    }

    /// The counts of view `view` in three fixed slots ([`count_slots`]);
    /// `empty`: *nothing to show*, right-aligned in the same box.
    pub(super) fn render_counts(
        reference: &DashboardRef,
        view: &View,
        counts: &Summary,
        empty: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let width = counts_width(theme);
        if empty {
            return div()
                .flex()
                .flex_none()
                .justify_end()
                .w(width)
                .text_color(colors.text_faint)
                .child("nothing to show")
                .into_any_element();
        }
        let slots = count_slots(counts, view);
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SLOT_GAP))
            .w(width)
            .children(
                slots
                    .into_iter()
                    .enumerate()
                    .map(|(position, slot)| Self::count_slot(reference, view, position, slot, cx)),
            )
            .into_any_element()
    }

    /// Count slot `position` of `view`: its dot and count, nothing at zero;
    /// a list's is its state chip (picked: the accent border).
    fn count_slot(
        reference: &DashboardRef,
        view: &View,
        position: usize,
        slot: Option<CountSlot>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let base = div()
            .id(SharedString::from(format!(
                "view-count-{position}:{}",
                view.id
            )))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SLOT_DOT_GAP))
            .w(slot_width(theme))
            .h(px(20.))
            .px(px(SLOT_PADDING))
            .rounded(theme.metrics.small_radius)
            .border_1()
            .border_color(gpui::transparent_black());
        let Some(slot) = slot else {
            return base.into_any_element();
        };
        let picked = slot.chip.is_some() && view.state == slot.chip;
        let shown = slot.count > 0 || picked;
        let clickable = slot.chip.is_some() && shown;
        let chip = slot.chip;
        let reference = reference.clone();
        let view_id = view.id.clone();
        base.when(picked, |base| {
            base.border_color(colors.accent)
                .bg(colors.accent_tint)
                .text_color(colors.accent_text)
        })
        .when(shown, |base| {
            base.child(StateDot::new(slot.state).size(px(SLOT_DOT)))
                .child(slot.count.to_string())
        })
        .when(clickable, |base| {
            base.cursor_pointer()
                .hover(|style| style.bg(colors.element_hover))
                .tooltip(Tooltip::text(if picked {
                    format!("Showing only {} · click to show all", slot.word)
                } else {
                    format!("{} {} · click to show only these", slot.count, slot.word)
                }))
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let next = if picked { None } else { chip };
                    this.change_view(&reference, &view_id, move |view| view.state = next, cx);
                    cx.notify();
                }))
        })
        .into_any_element()
    }

    /// The chips of a handling or downtimes view in its header (`narrow`:
    /// marks and counts, the words in the tooltips).
    pub(super) fn render_thread_chips(
        reference: &DashboardRef,
        view: &View,
        thread: &ThreadPage,
        narrow: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let kind = thread.kind;
        let summary = &thread.listing.summary;
        let chips = kind.chips().iter().map(|&chip| {
            let selected = chip == thread.options.chip;
            let count = summary.count(chip);
            let reference = reference.clone();
            let view_id = view.id.clone();
            let threads = view.threads;
            let word = match count {
                Some(count) => format!("{count} {}", chip.label(kind)),
                None => chip.label(kind).to_owned(),
            };
            crate::lists::view::thread_chip(
                SharedString::from(format!("view-chip-{}:{}", chip.id(), view.id)),
                chip,
                kind,
                count,
                narrow,
                selected,
                theme,
            )
            .when(narrow, |chip| chip.tooltip(Tooltip::text(word)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                cx.stop_propagation();
                let mut options = Options::of_view(kind, threads);
                options.pick_chip(chip);
                let threads = options.to_view();
                this.change_view(&reference, &view_id, move |view| view.threads = threads, cx);
                cx.notify();
            }))
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(CHIP_GAP))
            .children(chips)
            .into_any_element()
    }

    /// The downtimes view's `timeline | list` (`compact`: in a stacked
    /// header), saved with the view (the editor's *opens as*).
    pub(super) fn render_mode_switch(
        reference: &DashboardRef,
        view: &View,
        compact: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let options = Options::of_view(ListKind::Downtimes, view.threads);
        let selected = match options.mode {
            Mode::Timeline => 0,
            Mode::List => 1,
        };
        let reference = reference.clone();
        let view_id = view.id.clone();
        let threads = view.threads;
        let switch = Segmented::new(SharedString::from(format!("view-mode:{}", view.id)))
            .option("timeline")
            .option("list")
            .hug()
            .selected(selected)
            .on_select(cx.listener(move |this, index: &usize, _, cx| {
                let mode = if *index == 0 {
                    Mode::Timeline
                } else {
                    Mode::List
                };
                let mut options = Options::of_view(ListKind::Downtimes, threads);
                options.pick_mode(mode);
                let threads = options.to_view();
                this.change_view(&reference, &view_id, move |view| view.threads = threads, cx);
                cx.notify();
            }));
        div()
            .flex_none()
            .child(if compact { switch.compact() } else { switch })
            .into_any_element()
    }

    /// The sort of a handling or downtimes view (`latest activity ↓`),
    /// sized to its word; its menu hangs from the header's right edge.
    pub(super) fn thread_sort_trigger(
        &self,
        view: &View,
        kind: ListKind,
        menu: HeaderMenu,
        cx: &Context<Self>,
    ) -> AnyElement {
        let options = Options::of_view(kind, view.threads);
        let label = options.sort(kind).label(kind, options.chip);
        let open = self.menus.open() == Some(menu);
        let id = match menu {
            HeaderMenu::ViewSort(index) => SharedString::from(format!("view-sort-{index}")),
            _ => SharedString::from("sort-trigger"),
        };
        div()
            .flex_none()
            .child(sort_word(id, label, open, menu, cx))
            .into_any_element()
    }

    /// A handling or downtimes view's sort menu.
    pub(super) fn thread_sort_menu(
        reference: &DashboardRef,
        view: &View,
        kind: ListKind,
        cx: &Context<Self>,
    ) -> Menu {
        let options = Options::of_view(kind, view.threads);
        let current = options.sort(kind);
        let mut menu = Menu::new("view-thread-sort-menu").label("sort by");
        for &choice in kind.sorts() {
            let reference = reference.clone();
            let view_id = view.id.clone();
            let threads = view.threads;
            menu = menu.item(
                MenuItem::new(choice.id(), choice.menu_label(kind))
                    .checked(choice == current)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        let mut options = Options::of_view(kind, threads);
                        options.sort = Some(choice);
                        let threads = options.to_view();
                        this.change_view(
                            &reference,
                            &view_id,
                            move |view| view.threads = threads,
                            cx,
                        );
                        cx.notify();
                    })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    /// The *only mine* item of a handling or downtimes view's `···`.
    pub(super) fn only_mine_item(
        &self,
        reference: &DashboardRef,
        view: &View,
        kind: ListKind,
        cx: &Context<Self>,
    ) -> MenuItem {
        let denial = self.state.read(cx).only_mine_denial(kind);
        let on = view.threads.only_mine && denial.is_none();
        let reference = reference.clone();
        let view_id = view.id.clone();
        MenuItem::new("view-only-mine", "only mine")
            .checked(on)
            .disabled(denial.is_some())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.menus.close();
                this.change_view(
                    &reference,
                    &view_id,
                    move |view| view.threads.only_mine = !on,
                    cx,
                );
                cx.notify();
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_keep_three_slots_and_lists_pick_states() {
        let summary = Summary {
            critical: 427,
            unknown: 2,
            ok: 30,
            ..Summary::default()
        };
        let list = View::default();
        let slots = count_slots(&summary, &list);
        assert_eq!(slots[0].map(|slot| slot.count), Some(427));
        assert_eq!(
            slots[1].map(|slot| slot.count),
            Some(0),
            "a state at zero keeps its slot"
        );
        assert_eq!(
            slots[2].and_then(|slot| slot.chip),
            Some(StateChip::Unknown)
        );
        let hosts = View {
            object_kind: ObjectKind::Hosts,
            ..View::default()
        };
        let down = Summary {
            down: 3,
            ..Summary::default()
        };
        let slots = count_slots(&down, &hosts);
        assert_eq!(slots[0].and_then(|slot| slot.chip), Some(StateChip::Down));
        assert!(slots[1].is_none(), "hosts have no yellow state");
        // A quiet list shows what it lists, not clickable.
        let quiet = Summary {
            ok: 12,
            ..Summary::default()
        };
        let slots = count_slots(&quiet, &list);
        assert_eq!(
            slots[0].map(|slot| (slot.count, slot.chip)),
            Some((12, None))
        );
        assert!(slots[1].is_none());
        // A grid mixes hosts and services, never clickable.
        let grid = View {
            display: ic_config::ViewDisplay::HostGroupGrid,
            ..View::default()
        };
        let mixed = Summary {
            critical: 1,
            down: 2,
            ..Summary::default()
        };
        let slots = count_slots(&mixed, &grid);
        assert_eq!(
            slots[0].map(|slot| (slot.count, slot.chip)),
            Some((3, None))
        );
    }

    #[test]
    fn slots_are_sized_for_three_digits() {
        let theme = Theme::dark();
        assert!(counts_width(&theme) > slot_width(&theme) * 3.);
        assert!(
            chips_width(ListKind::Handling, true, &theme)
                < chips_width(ListKind::Handling, false, &theme)
        );
    }
}
