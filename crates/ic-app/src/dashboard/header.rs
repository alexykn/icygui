//! The list header (`production  service problems … severity ↓ ···`) with
//! its sort and options menus, and the summary bar under it.

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, Pixels, Point, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _,
};
use ic_config::{GroupBy, ObjectKind, Sort, SortKey, View};
use ic_core::snapshot::Summary;
use ic_model::{CheckableState, HostState, ServiceState};
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, GlyphButton, Menu, MenuItem, PaneHeader, Popover, SummaryBar, SummaryItem,
    Tooltip,
};

use super::DashboardView;
use crate::chrome::Controls;
use crate::workspace::sidebar_reopen;

/// The header's popup menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderMenu {
    /// `severity ↓`: sort key and direction.
    Sort,
    /// `···`: grouping, the handled toggle, copying the filter.
    Options,
}

/// Which header menu is open.
///
/// A press outside an open menu closes it; when that press is on the
/// menu's own trigger, the trigger's click must not open it again, so the
/// press that closed a menu is remembered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct HeaderMenus {
    open: Option<HeaderMenu>,
    dismissed: Option<(HeaderMenu, Point<Pixels>)>,
}

impl HeaderMenus {
    /// The open menu.
    pub(super) fn open(&self) -> Option<HeaderMenu> {
        self.open
    }

    /// A click on `menu`'s trigger that went down at `down`.
    pub(super) fn toggle(&mut self, menu: HeaderMenu, down: Option<Point<Pixels>>) {
        let dismissed = self.dismissed.take();
        if down.is_some() && dismissed == down.map(|down| (menu, down)) {
            // This press already closed the menu.
            return;
        }
        self.open = if self.open == Some(menu) {
            None
        } else {
            Some(menu)
        };
    }

    /// A press at `at` outside the open menu.
    pub(super) fn dismiss(&mut self, at: Point<Pixels>) {
        if let Some(menu) = self.open.take() {
            self.dismissed = Some((menu, at));
        }
    }

    /// Closes the open menu. Returns whether one was open.
    pub(super) fn close(&mut self) -> bool {
        self.dismissed = None;
        self.open.take().is_some()
    }
}

/// Where a mouse click went down.
fn down_position(event: &ClickEvent) -> Option<Point<Pixels>> {
    match event {
        ClickEvent::Mouse(click) => Some(click.down.position),
        ClickEvent::Keyboard(_) | ClickEvent::Touch(_) => None,
    }
}

impl DashboardView {
    pub(super) fn render_header(
        &self,
        reference: Option<&DashboardRef>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let controls = Controls::of(window, cx);
        let mut header = PaneHeader::new("main-header").padding(theme.metrics.list_padding);
        if !self.sidebar_open {
            header = header.leading(sidebar_reopen(controls, theme));
        }
        let dashboard = reference.and_then(|reference| state.dashboard(reference));
        let header = match (reference, dashboard) {
            (Some(reference), Some((_, dashboard))) => header
                .title(dashboard.name.clone())
                .subtitle(view_label(&dashboard.view))
                .child(self.sort_trigger(reference, &dashboard.view, cx))
                .child(self.options_trigger(reference, &dashboard.view, cx)),
            _ => header.title("icygui"),
        };
        self.drag
            .attach(div().id("main-header-drag").child(header), controls)
            .into_any_element()
    }

    fn sort_trigger(
        &self,
        reference: &DashboardRef,
        view: &View,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.open() == Some(HeaderMenu::Sort);
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id("sort-trigger")
                    .text_size(theme.text.small)
                    .text_color(if open { colors.text } else { colors.text_muted })
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text))
                    .child(sort_label(view.sort, view.object_kind))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                        this.menus.toggle(HeaderMenu::Sort, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                trigger.child(Popover::new(Self::sort_menu(reference, view, cx)).align_right())
            })
            .into_any_element()
    }

    fn sort_menu(reference: &DashboardRef, view: &View, cx: &Context<Self>) -> Menu {
        let keys = [
            (SortKey::Severity, "severity"),
            (SortKey::LastStateChange, "last state change"),
            (SortKey::Host, "host"),
            (
                SortKey::Service,
                match view.object_kind {
                    ObjectKind::Services => "service",
                    ObjectKind::Hosts => "name",
                },
            ),
        ];
        let set_sort = |id: &'static str, label: &'static str, checked: bool, sort: Sort| {
            let reference = reference.clone();
            MenuItem::new(id, label)
                .checked(checked)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.state.update(cx, |state, cx| {
                        if state.update_view(&reference, |view| view.sort = sort) {
                            cx.notify();
                        }
                    });
                    cx.notify();
                }))
        };
        let mut menu = Menu::new("sort-menu").label("sort by");
        for (key, label) in keys {
            let sort = Sort {
                key,
                descending: if key == view.sort.key {
                    view.sort.descending
                } else {
                    natural_descending(key)
                },
            };
            menu = menu.item(set_sort(sort_id(key), label, key == view.sort.key, sort));
        }
        menu.separator()
            .item(set_sort(
                "sort-descending",
                "descending ↓",
                view.sort.descending,
                Sort {
                    descending: true,
                    ..view.sort
                },
            ))
            .item(set_sort(
                "sort-ascending",
                "ascending ↑",
                !view.sort.descending,
                Sort {
                    descending: false,
                    ..view.sort
                },
            ))
            .on_dismiss(Self::dismiss_listener(cx))
    }

    fn options_trigger(
        &self,
        reference: &DashboardRef,
        view: &View,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let open = self.menus.open() == Some(HeaderMenu::Options);
        let trigger = GlyphButton::new("dashboard-options", "···")
            .text_size(gpui::px(13.))
            .bleed()
            .color(theme.colors.text_muted)
            .selected(open)
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menus.toggle(HeaderMenu::Options, down_position(event));
                cx.notify();
            }));
        div()
            .relative()
            .flex_none()
            // The open menu says what the button is.
            .child(if open {
                trigger
            } else {
                trigger.tooltip(Tooltip::new("Dashboard options"))
            })
            .when(open, |trigger| {
                trigger.child(Popover::new(Self::options_menu(reference, view, cx)).align_right())
            })
            .into_any_element()
    }

    fn options_menu(reference: &DashboardRef, view: &View, cx: &Context<Self>) -> Menu {
        let update = |change: Box<dyn Fn(&mut View)>| {
            let reference = reference.clone();
            cx.listener(move |this: &mut Self, _: &ClickEvent, _, cx| {
                this.menus.close();
                this.state.update(cx, |state, cx| {
                    if state.update_view(&reference, |view| change(view)) {
                        cx.notify();
                    }
                });
                cx.notify();
            })
        };
        let groupings = [
            (GroupBy::None, "group-none", "none"),
            (GroupBy::Host, "group-host", "host"),
            (GroupBy::HostGroup, "group-host-group", "host group"),
            (
                GroupBy::ServiceGroup,
                "group-service-group",
                "service group",
            ),
        ];
        let mut menu = Menu::new("options-menu").label("group by");
        for (group_by, id, label) in groupings {
            if group_by == GroupBy::ServiceGroup && view.object_kind == ObjectKind::Hosts {
                continue;
            }
            menu = menu.item(
                MenuItem::new(id, label)
                    .checked(view.group_by == group_by)
                    .on_click(update(Box::new(move |view: &mut View| {
                        view.group_by = group_by;
                    }))),
            );
        }
        let hide = !view.hide_handled;
        let filter = view.filter.clone();
        menu.separator()
            .item(
                MenuItem::new("toggle-handled", "hide handled problems")
                    .checked(view.hide_handled)
                    .on_click(update(Box::new(move |view: &mut View| {
                        view.hide_handled = hide;
                    }))),
            )
            .item(
                MenuItem::new("copy-filter", "copy filter expression")
                    .disabled(filter.trim().is_empty())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        cx.write_to_clipboard(ClipboardItem::new_string(filter.clone()));
                        cx.notify();
                    })),
            )
            .on_dismiss(Self::dismiss_listener(cx))
    }

    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
        cx.listener(|this, event: &MouseDownEvent, _, cx| {
            this.menus.dismiss(event.position);
            cx.notify();
        })
    }

    /// The summary bar for a list `list_width` wide: where the labels
    /// don't fit, the items show their counts only.
    pub(super) fn render_summary(
        &self,
        reference: &DashboardRef,
        list_width: Pixels,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let (_, dashboard) = state.dashboard(reference)?;
        let result = state.result(reference)?;
        let view = &dashboard.view;
        let items = summary_items(&result.summary, view.object_kind);
        if items.is_empty() {
            // Nothing matches the filter: the empty state says so.
            return None;
        }
        let marked = self
            .lists
            .get(reference)
            .map_or(0, |list| list.selection.marked_count());
        let toggle_reference = reference.clone();
        let hide = !view.hide_handled;
        let handled_label = if view.hide_handled {
            "handled hidden"
        } else {
            "handled shown"
        };
        let end_text = if marked > 0 {
            // The 14px gap is about two characters.
            format!("{marked} selected  {handled_label}")
        } else {
            handled_label.to_owned()
        };
        let texts: Vec<String> = items
            .iter()
            .map(|(state, count, label)| SummaryItem::new(*state, *count, *label).text())
            .collect();
        let compact = !SummaryBar::fits(
            texts.iter().map(String::as_str),
            &end_text,
            list_width,
            theme,
        );
        let end = div()
            .flex()
            .items_center()
            .gap(gpui::px(14.))
            .when(marked > 0, |end| {
                end.child(
                    div()
                        .text_color(colors.accent)
                        .child(format!("{marked} selected")),
                )
            })
            .child(
                div()
                    .id("handled-toggle")
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text_muted))
                    .child(handled_label)
                    .tooltip(Tooltip::text(if view.hide_handled {
                        "Show acknowledged problems, downtimes and problems on down hosts"
                    } else {
                        "Hide acknowledged problems, downtimes and problems on down hosts"
                    }))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        let reference = toggle_reference.clone();
                        this.state.update(cx, |state, cx| {
                            if state.update_view(&reference, |view| view.hide_handled = hide) {
                                cx.notify();
                            }
                        });
                    })),
            );
        Some(
            SummaryBar::new()
                .children(items.into_iter().map(|(state, count, label)| {
                    SummaryItem::new(state, count, label).compact(compact)
                }))
                .end(end)
                .into_any_element(),
        )
    }
}

/// The element id of a sort key's menu item.
fn sort_id(key: SortKey) -> &'static str {
    match key {
        SortKey::Severity => "sort-severity",
        SortKey::LastStateChange => "sort-last-state-change",
        SortKey::Host => "sort-host",
        SortKey::Service => "sort-service",
    }
}

/// The direction a key sorts in when chosen: worst and newest first, names
/// from A.
fn natural_descending(key: SortKey) -> bool {
    matches!(key, SortKey::Severity | SortKey::LastStateChange)
}

/// What a view lists, as the header's subtitle.
pub(super) fn view_label(view: &View) -> &'static str {
    match (view.object_kind, view.problems_only) {
        (ObjectKind::Services, true) => "service problems",
        (ObjectKind::Hosts, true) => "host problems",
        (ObjectKind::Services, false) => "services",
        (ObjectKind::Hosts, false) => "hosts",
    }
}

/// The sort trigger's text: `severity ↓`.
pub(super) fn sort_label(sort: Sort, kind: ObjectKind) -> SharedString {
    let key = match (sort.key, kind) {
        (SortKey::Severity, _) => "severity",
        (SortKey::LastStateChange, _) => "last change",
        (SortKey::Host, _) => "host",
        (SortKey::Service, ObjectKind::Services) => "service",
        (SortKey::Service, ObjectKind::Hosts) => "name",
    };
    let arrow = if sort.descending { "↓" } else { "↑" };
    format!("{key} {arrow}").into()
}

/// The counts the summary bar shows: problem states with a count, or the OK
/// count when there are none, then pending objects. Empty when nothing
/// matches the dashboard's filter.
fn summary_items(summary: &Summary, kind: ObjectKind) -> Vec<(CheckableState, u32, &'static str)> {
    let (problems, ok, pending) = match kind {
        ObjectKind::Services => (
            vec![
                (
                    CheckableState::Service(ServiceState::Critical),
                    summary.critical,
                    "critical",
                ),
                (
                    CheckableState::Service(ServiceState::Warning),
                    summary.warning,
                    "warning",
                ),
                (
                    CheckableState::Service(ServiceState::Unknown),
                    summary.unknown,
                    "unknown",
                ),
            ],
            (CheckableState::Service(ServiceState::Ok), "ok"),
            CheckableState::Service(ServiceState::Pending),
        ),
        ObjectKind::Hosts => (
            vec![
                (CheckableState::Host(HostState::Down), summary.down, "down"),
                (
                    CheckableState::Host(HostState::Unreachable),
                    summary.unreachable,
                    "unreachable",
                ),
            ],
            (CheckableState::Host(HostState::Up), "up"),
            CheckableState::Host(HostState::Pending),
        ),
    };
    let mut items: Vec<_> = problems
        .into_iter()
        .filter(|(_, count, _)| *count > 0)
        .collect();
    if items.is_empty() && summary.ok > 0 {
        items.push((ok.0, summary.ok, ok.1));
    }
    if summary.pending > 0 {
        items.push((pending, summary.pending, "pending"));
    }
    items
}

#[cfg(test)]
mod tests {
    use gpui::{point, px};

    use super::*;

    fn view(kind: ObjectKind, problems_only: bool) -> View {
        View {
            object_kind: kind,
            problems_only,
            ..View::default()
        }
    }

    #[test]
    fn views_are_described_by_kind_and_problems() {
        assert_eq!(
            view_label(&view(ObjectKind::Services, true)),
            "service problems"
        );
        assert_eq!(view_label(&view(ObjectKind::Hosts, true)), "host problems");
        assert_eq!(view_label(&view(ObjectKind::Hosts, false)), "hosts");
    }

    #[test]
    fn sort_labels_show_key_and_direction() {
        assert_eq!(
            sort_label(Sort::default(), ObjectKind::Services),
            "severity ↓"
        );
        let by_name = Sort {
            key: SortKey::Service,
            descending: false,
        };
        assert_eq!(sort_label(by_name, ObjectKind::Hosts), "name ↑");
        assert_eq!(sort_label(by_name, ObjectKind::Services), "service ↑");
        assert!(natural_descending(SortKey::LastStateChange));
        assert!(!natural_descending(SortKey::Host));
    }

    #[test]
    fn the_summary_lists_problem_states_with_counts() {
        let summary = Summary {
            critical: 12,
            warning: 29,
            unknown: 0,
            ok: 400,
            ..Summary::default()
        };
        let items = summary_items(&summary, ObjectKind::Services);
        let labels: Vec<_> = items
            .iter()
            .map(|(_, count, label)| (*count, *label))
            .collect();
        assert_eq!(labels, [(12, "critical"), (29, "warning")]);
    }

    #[test]
    fn a_quiet_summary_shows_the_ok_count() {
        let summary = Summary {
            ok: 7,
            ..Summary::default()
        };
        let items = summary_items(&summary, ObjectKind::Hosts);
        assert_eq!(items, [(CheckableState::Host(HostState::Up), 7, "up")]);
    }

    #[test]
    fn pending_objects_are_counted_and_an_empty_summary_shows_nothing() {
        let summary = Summary {
            warning: 2,
            ok: 30,
            pending: 3,
            ..Summary::default()
        };
        let items = summary_items(&summary, ObjectKind::Services);
        assert_eq!(
            items,
            [
                (CheckableState::Service(ServiceState::Warning), 2, "warning"),
                (CheckableState::Service(ServiceState::Pending), 3, "pending"),
            ]
        );
        let only_pending = Summary {
            pending: 1,
            ..Summary::default()
        };
        assert_eq!(
            summary_items(&only_pending, ObjectKind::Hosts),
            [(CheckableState::Host(HostState::Pending), 1, "pending")]
        );
        assert!(summary_items(&Summary::default(), ObjectKind::Services).is_empty());
    }

    #[test]
    fn menus_toggle_and_close() {
        let mut menus = HeaderMenus::default();
        menus.toggle(HeaderMenu::Sort, Some(point(px(1.), px(1.))));
        assert_eq!(menus.open(), Some(HeaderMenu::Sort));
        menus.toggle(HeaderMenu::Options, None);
        assert_eq!(menus.open(), Some(HeaderMenu::Options), "switches menus");
        assert!(menus.close());
        assert!(!menus.close());
    }

    #[test]
    fn the_press_that_closed_a_menu_does_not_reopen_it() {
        let mut menus = HeaderMenus::default();
        let press = point(px(500.), px(20.));
        menus.toggle(HeaderMenu::Sort, Some(point(px(1.), px(1.))));
        // Pressing the trigger again: the menu's outside handler runs first…
        menus.dismiss(press);
        assert_eq!(menus.open(), None);
        // …then the trigger's click for the same press.
        menus.toggle(HeaderMenu::Sort, Some(press));
        assert_eq!(menus.open(), None, "stays closed");
        // The next click opens it.
        menus.toggle(HeaderMenu::Sort, Some(point(px(501.), px(20.))));
        assert_eq!(menus.open(), Some(HeaderMenu::Sort));
    }

    #[test]
    fn closing_one_menu_by_opening_another_works() {
        let mut menus = HeaderMenus::default();
        let press = point(px(600.), px(20.));
        menus.toggle(HeaderMenu::Sort, None);
        menus.dismiss(press);
        menus.toggle(HeaderMenu::Options, Some(press));
        assert_eq!(menus.open(), Some(HeaderMenu::Options));
    }

    #[test]
    fn dismissing_elsewhere_doesnt_swallow_the_next_click() {
        let mut menus = HeaderMenus::default();
        menus.toggle(HeaderMenu::Sort, None);
        menus.dismiss(point(px(10.), px(500.)));
        menus.toggle(HeaderMenu::Sort, Some(point(px(500.), px(20.))));
        assert_eq!(menus.open(), Some(HeaderMenu::Sort));
    }
}
