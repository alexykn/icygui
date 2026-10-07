//! The dashboard's header (`production  service problems … severity ↓ ···`;
//! with several views `databases  4 views … ···`) with its menus, the
//! summary bar of a single list, and the views' headers (topic 04) with
//! their own sort and `···`. Lists show their handled problems' button in
//! a fixed slot: `28 hidden · show`, `28 handled · hide` (2j, 4a).

use gpui::{
    AnyElement, ClickEvent, ClipboardItem, Context, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, Pixels, Point, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, prelude::FluentBuilder as _,
};
use ic_config::{GroupBy, HideHandled, ObjectKind, Sort, SortKey, View};
use ic_core::snapshot::{Summary, ViewResult};
use ic_model::{CheckableState, HostState, ServiceState};
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, Dismissal, GlyphButton, Icon, IconName, Menu, MenuItem, PaneHeader, Popover,
    StateDot, SummaryBar, SummaryItem, Tooltip, px,
};

use super::DashboardView;
use crate::app_state::connection::ViewMarker;
use crate::chrome::Controls;
use crate::workspace::sidebar_reopen;

/// The header's popup menus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HeaderMenu {
    /// `severity ↓`: sort key and direction.
    Sort,
    /// `···`: grouping, the handled toggle, copying the filter.
    Options,
    /// The selection bar's `···`: more bulk actions, copying.
    Selection,
    /// A view header's sort (by the view's index): `sort this view by`.
    ViewSort(usize),
    /// A view header's `···`.
    ViewOptions(usize),
}

/// Which header menu is open.
///
/// Escape or a press outside an open menu closes it; when that press is on
/// the menu's own trigger, the trigger's click must not open it again, so
/// the press that closed a menu is remembered.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct HeaderMenus {
    open: Option<HeaderMenu>,
    dismissed: Option<(HeaderMenu, Point<Pixels>)>,
}

impl HeaderMenus {
    /// The open menu.
    pub(crate) fn open(&self) -> Option<HeaderMenu> {
        self.open
    }

    /// A click on `menu`'s trigger that went down at `down`.
    pub(crate) fn toggle(&mut self, menu: HeaderMenu, down: Option<Point<Pixels>>) {
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
    pub(crate) fn dismiss(&mut self, at: Point<Pixels>) {
        if let Some(menu) = self.open.take() {
            self.dismissed = Some((menu, at));
        }
    }

    /// The open menu closed by itself: a press outside it, or Escape.
    pub(crate) fn dismissed(&mut self, how: Dismissal) {
        match how {
            Dismissal::Press(at) => self.dismiss(at),
            Dismissal::Escape => {
                self.close();
            }
        }
    }

    /// Closes the open menu. Returns whether one was open.
    pub(crate) fn close(&mut self) -> bool {
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
        // `--demo` says so over every dashboard of its own environments
        // (ENV-10); the footer's room goes to the environment switcher.
        if state.is_demo_environment() {
            header = header.child(demo_chip(theme));
        }
        let header = match (reference, dashboard) {
            (Some(reference), Some((_, dashboard))) if super::is_multi_view(&dashboard.views) => {
                // Several views: each sorts on its own and counts in its
                // header; the dashboard's header names them.
                let count = dashboard.views.len();
                let filter = self.pages.get(reference).and_then(|ui| ui.filter.clone());
                header
                    .title(dashboard.name.clone())
                    .subtitle(if count == 1 {
                        "1 view".to_owned()
                    } else {
                        format!("{count} views")
                    })
                    .children(filter.map(|filter| Self::filter_chip(&filter, cx)))
                    .child(self.dashboard_options_trigger(reference, cx))
            }
            (Some(reference), Some((_, dashboard))) => {
                let view = super::primary_view(&dashboard.views);
                header
                    .title(dashboard.name.clone())
                    .subtitle(view_label(view))
                    .child(self.sort_trigger(reference, view, HeaderMenu::Sort, cx))
                    .child(self.options_trigger(reference, view, cx))
            }
            _ => header.title("icygui"),
        };
        self.drag
            .attach(div().id("main-header-drag").child(header), controls)
            .into_any_element()
    }

    /// A sort trigger (`severity ↓`) opening `menu`: the single list's in
    /// the dashboard's header, a view's in its header.
    pub(super) fn sort_trigger(
        &self,
        reference: &DashboardRef,
        view: &View,
        menu: HeaderMenu,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.open() == Some(menu);
        let id = match menu {
            HeaderMenu::ViewSort(index) => SharedString::from(format!("view-sort-{index}")),
            _ => SharedString::from("sort-trigger"),
        };
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(id)
                    .text_size(theme.text.small)
                    .text_color(if open { colors.text } else { colors.text_muted })
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text))
                    .child(sort_label(view.sort, view.object_kind))
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.menus.toggle(menu, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                trigger
                    .child(Popover::new(Self::sort_menu(reference, view, menu, cx)).align_right())
            })
            .into_any_element()
    }

    fn sort_menu(
        reference: &DashboardRef,
        view: &View,
        menu: HeaderMenu,
        cx: &Context<Self>,
    ) -> Menu {
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
        let view_id = view.id.clone();
        let set_sort = |id: &'static str, label: &'static str, checked: bool, sort: Sort| {
            let reference = reference.clone();
            let view_id = view_id.clone();
            MenuItem::new(id, label)
                .checked(checked)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.state.update(cx, |state, cx| {
                        if state.update_view(&reference, &view_id, |view| view.sort = sort) {
                            cx.notify();
                        }
                    });
                    cx.notify();
                }))
        };
        let mut built = Menu::new("sort-menu").label(match menu {
            HeaderMenu::ViewSort(_) => "sort this view by",
            _ => "sort by",
        });
        for (key, label) in keys {
            let sort = Sort {
                key,
                descending: if key == view.sort.key {
                    view.sort.descending
                } else {
                    natural_descending(key)
                },
            };
            built = built.item(set_sort(sort_id(key), label, key == view.sort.key, sort));
        }
        built
            .separator()
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
        let defaults = self.state.read(cx).handled_defaults();
        let trigger = GlyphButton::new("dashboard-options", "···")
            .text_size(px(13.))
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
                trigger.child(
                    Popover::new(Self::options_menu(reference, view, defaults, cx)).align_right(),
                )
            })
            .into_any_element()
    }

    pub(super) fn options_menu(
        reference: &DashboardRef,
        view: &View,
        defaults: HideHandled,
        cx: &Context<Self>,
    ) -> Menu {
        let update = |change: Box<dyn Fn(&mut View)>| {
            let reference = reference.clone();
            let view_id = view.id.clone();
            cx.listener(move |this: &mut Self, _: &ClickEvent, _, cx| {
                this.menus.close();
                this.state.update(cx, |state, cx| {
                    if state.update_view(&reference, &view_id, |view| change(view)) {
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
        let edit = reference.clone();
        let mut menu = Menu::new("options-menu")
            .item(
                MenuItem::new("edit-dashboard", "edit dashboard").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        cx.emit(super::DashboardEvent::Edit(edit.clone()));
                        cx.notify();
                    },
                )),
            )
            .separator()
            .label("group by");
        for (group_by, id, label) in groupings {
            if group_by == GroupBy::ServiceGroup && view.object_kind == ObjectKind::Hosts {
                continue;
            }
            menu = menu.item(
                MenuItem::new(id, label)
                    .checked(view.list_grouping() == group_by)
                    .on_click(update(Box::new(move |view: &mut View| {
                        view.set_grouping(group_by);
                    }))),
            );
        }
        let hiding = view.hidden_handled(defaults).any();
        let filter = view.filter.clone();
        menu.separator()
            .item(
                MenuItem::new("toggle-handled", "hide handled problems")
                    .checked(hiding)
                    .on_click(update(Box::new(move |view: &mut View| {
                        view.handled = view.handled.toggled(defaults);
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

    pub(super) fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&Dismissal, &mut Window, &mut gpui::App) + 'static {
        cx.listener(|this, dismissal: &Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }

    /// The summary bar of a single list `list_width` wide (a dashboard
    /// with several views has none: each view header counts): where the
    /// labels don't fit, the items show their counts only. Its handled
    /// slot is at the right end, fixed and right-aligned.
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
        if super::is_multi_view(&dashboard.views) {
            return None;
        }
        let view = super::primary_view(&dashboard.views);
        let result = state.view_result(reference, &view.id)?;
        // The bar counts the unhandled problems the list is about (2j:
        // showing or hiding handled ones never changes them; the sidebar
        // counts the same); a list of only OK objects counts those.
        let items = summary_items(&result.counts, view.object_kind);
        let items = if items.is_empty() {
            summary_items(&result.shown, view.object_kind)
        } else {
            items
        };
        if items.is_empty() {
            // Nothing is listed: the empty state says why.
            return None;
        }
        // Counts from a node that sees part of the cluster never look
        // complete (ENV-12): the view's label before the slot.
        let marker = state
            .snapshot()
            .node
            .as_ref()
            .and_then(|node| ViewMarker::of(&node.view));
        let slot = handled_slot_width(theme);
        let end_text = match &marker {
            // The 14 px gap is about two characters wide.
            Some(marker) => format!("{}  {}", marker.label, " ".repeat(HANDLED_SLOT_CHARS)),
            None => " ".repeat(HANDLED_SLOT_CHARS),
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
        let view_label = marker.map(|marker| {
            div()
                .id("view-marker")
                .text_color(if marker.partial {
                    cx.theme().states.text.warning
                } else {
                    colors.text_faint
                })
                .child(marker.label)
                .tooltip(Tooltip::text(marker.detail))
        });
        let end = div()
            .flex()
            .items_center()
            .gap(px(14.))
            .children(view_label)
            .child(
                div()
                    .flex()
                    .justify_end()
                    .w(slot)
                    .children(Self::handled_slot(
                        reference,
                        &view.id,
                        result,
                        "handled-toggle",
                        cx,
                    )),
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

    /// The handled button of a list (summary bar, view header): `28 hidden
    /// · show` while handled problems are hidden, `28 handled · hide`
    /// while they show (hollow); nothing when none are handled. Its slot is
    /// the caller's, fixed and right-aligned, so nothing moves.
    pub(super) fn handled_slot(
        reference: &DashboardRef,
        view_id: &str,
        result: &ViewResult,
        id: &'static str,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let theme = cx.theme();
        let colors = theme.colors;
        let (count, verb) = handled_text(result.hidden, result.handled)?;
        let reference = reference.clone();
        let view_id = view_id.to_owned();
        let hiding = result.hidden > 0;
        Some(
            div()
                .id(SharedString::from(format!("{id}:{view_id}")))
                .flex()
                .flex_none()
                .items_center()
                .whitespace_nowrap()
                .text_size(theme.text.small)
                .text_color(colors.text_faint)
                .cursor_pointer()
                .child(count)
                .child(" · ")
                .child(div().text_color(colors.accent_text).child(verb))
                .tooltip(Tooltip::text(if hiding {
                    "Show the handled problems: acknowledged, in downtime, on hosts that are down"
                } else {
                    "Hide the handled problems again"
                }))
                .on_mouse_down(MouseButton::Left, |_, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    let reference = reference.clone();
                    this.state.update(cx, |state, cx| {
                        if state.toggle_handled(&reference, &view_id) {
                            cx.notify();
                        }
                    });
                }))
                .into_any_element(),
        )
    }

    /// The `···` of a dashboard with several views: edit, copy.
    fn dashboard_options_trigger(
        &self,
        reference: &DashboardRef,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let open = self.menus.open() == Some(HeaderMenu::Options);
        let trigger = GlyphButton::new("dashboard-options", "···")
            .text_size(px(13.))
            .bleed()
            .color(theme.colors.text_muted)
            .selected(open)
            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                this.menus.toggle(HeaderMenu::Options, down_position(event));
                cx.notify();
            }));
        let edit = reference.clone();
        let menu = Menu::new("options-menu")
            .item(
                MenuItem::new("edit-dashboard", "edit dashboard").on_click(cx.listener(
                    move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        cx.emit(super::DashboardEvent::Edit(edit.clone()));
                        cx.notify();
                    },
                )),
            )
            .on_dismiss(Self::dismiss_listener(cx));
        div()
            .relative()
            .flex_none()
            .child(if open {
                trigger
            } else {
                trigger.tooltip(Tooltip::new("Dashboard options"))
            })
            .when(open, |trigger| {
                trigger.child(Popover::new(menu).align_right())
            })
            .into_any_element()
    }

    /// The header's chip for the page's group filter: `host group edge-ams
    /// ×` (filled); its × (or Esc) clears the filter.
    fn filter_chip(filter: &super::page::GroupFilter, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .id("group-filter-chip")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(22.))
            .px(px(8.))
            .rounded(theme.metrics.small_radius)
            .bg(colors.element_background)
            .text_size(theme.text.small)
            .text_color(colors.text)
            .child(filter.chip())
            .child(
                div()
                    .id("group-filter-clear")
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .child(
                        Icon::new(IconName::Close)
                            .size(px(11.))
                            .color(colors.text_faint),
                    )
                    .tooltip(Tooltip::new("Show every group").key("esc").builder())
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.clear_filter(cx))),
            )
            .into_any_element()
    }

    /// A view's header (topic 04): 36px on the pane's surface, a collapse
    /// chevron, the display's icon, the name, the filter (faint, cut off
    /// first), the counts (unhandled, or `nothing to show`), a list's
    /// handled slot, `live` for a stream, the view's own sort and `···`.
    /// The view holding the cursor has the accent bar and its name in
    /// strong text; the cursor on the header itself tints it.
    #[expect(
        clippy::too_many_lines,
        reason = "one header, its slots in reading order"
    )]
    pub(super) fn render_view_header(
        &self,
        reference: &DashboardRef,
        page: &super::page::Page,
        index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        use super::page::{Stop, ViewState};
        use ic_config::ViewDisplay;
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let Some(page_view) = page.views.get(index) else {
            return div().into_any_element();
        };
        let Some(view) = state
            .dashboard(reference)
            .and_then(|(_, dashboard)| dashboard.views.get(page_view.index))
        else {
            return div().into_any_element();
        };
        let ui = self.pages.get(reference);
        let stop = Stop::Header(page_view.id.clone());
        let focused = ui.and_then(super::PageUi::focused_view) == Some(index);
        let on_header = ui.and_then(|ui| ui.selection.cursor_stop()) == Some(&stop);
        let result = state.view_result(reference, &view.id);
        let chevron_stop = stop.clone();
        let chevron = div()
            .id(SharedString::from(format!("view-chevron:{}", view.id)))
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(12.))
            .h_full()
            .cursor_pointer()
            .child(
                Icon::new(if page_view.collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(px(12.))
                .color(colors.text_faint),
            )
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.click_chevron(&chevron_stop, window, cx);
            }));
        let icon = Icon::new(display_icon(view.display))
            .size(px(13.))
            .color(colors.text_muted);
        // The filter is cut off first; the name only when even that isn't
        // enough.
        let name = div()
            .min_w_0()
            .truncate()
            .text_size(theme.text.row)
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(if focused {
                colors.text_strong
            } else {
                colors.text_secondary
            })
            .child(if view.name.trim().is_empty() {
                view_label(view).to_owned()
            } else {
                view.name.clone()
            });
        let filter = div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(colors.text_faint)
            .child(filter_summary(view));
        let counts: Option<AnyElement> = match page_view.state {
            ViewState::Empty => Some(
                div()
                    .flex_none()
                    .text_color(colors.text_faint)
                    .child("nothing to show")
                    .into_any_element(),
            ),
            ViewState::Ready if view.display != ViewDisplay::EventStream => {
                let items = header_counts(&page_view.counts, view.object_kind, view.is_list());
                (!items.is_empty()).then(|| {
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(12.))
                        .children(items.into_iter().map(|(state, count)| {
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .child(StateDot::new(state).size(px(7.)))
                                .child(count.to_string())
                        }))
                        .into_any_element()
                })
            }
            _ => None,
        };
        // Lists keep a fixed, right-aligned slot for the handled button,
        // empty when nothing is handled, so the headers line up.
        let slot = view.is_list().then(|| {
            div()
                .flex()
                .flex_none()
                .justify_end()
                .w(handled_slot_width(theme))
                .children(result.and_then(|result| {
                    Self::handled_slot(reference, &view.id, result, "view-handled", cx)
                }))
        });
        let live = (view.display == ViewDisplay::EventStream).then(|| {
            let connected = state.connection().is_connected() && !state.rows_settling();
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(px(6.))
                .text_color(colors.text_faint)
                .when(!connected, gpui::Styled::invisible)
                .child(StateDot::new(CheckableState::Service(ServiceState::Ok)).size(px(6.)))
                .child("live")
        });
        let sort: AnyElement = match view.display {
            ViewDisplay::List | ViewDisplay::GroupedList => {
                self.sort_trigger(reference, view, HeaderMenu::ViewSort(index), cx)
            }
            ViewDisplay::HostGroupGrid | ViewDisplay::SummaryTiles => {
                self.group_order_trigger(reference, view, index, cx)
            }
            ViewDisplay::EventStream => div()
                .flex_none()
                .text_color(colors.text_muted)
                .child("newest first")
                .into_any_element(),
        };
        let more = self.view_options_trigger(reference, view, index, page_view.collapsed, cx);
        let click_stop = stop.clone();
        div()
            .id(SharedString::from(format!("view-header:{}", view.id)))
            .relative()
            .flex()
            .items_center()
            .gap(px(10.))
            .h_full()
            .pl(px(14.))
            .pr(theme.metrics.list_padding)
            .bg(if on_header {
                colors.row_selected
            } else {
                colors.pane_background
            })
            .border_b_1()
            .border_color(colors.border_header)
            .when(index > 0, gpui::Styled::border_t_1)
            .whitespace_nowrap()
            .text_size(theme.text.small)
            .text_color(colors.text_muted)
            .when(focused, |header| {
                header.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(colors.accent),
                )
            })
            .child(chevron)
            .child(icon)
            .child(name)
            .child(filter)
            .children(counts)
            .children(slot)
            .children(live)
            .child(sort)
            .child(more)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_stop(&click_stop, event.modifiers(), window, cx);
            }))
            .into_any_element()
    }

    /// A grid's or tiles' group order (`worst first`, `name ↑`) and its
    /// menu.
    fn group_order_trigger(
        &self,
        reference: &DashboardRef,
        view: &View,
        index: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        use ic_config::GroupOrder;
        let theme = cx.theme();
        let colors = theme.colors;
        let menu = HeaderMenu::ViewSort(index);
        let open = self.menus.open() == Some(menu);
        let label = match view.groups.order {
            GroupOrder::WorstFirst => "worst first",
            GroupOrder::Name => "name ↑",
        };
        let item = |id: &'static str, text: &'static str, order: GroupOrder| {
            let reference = reference.clone();
            let view_id = view.id.clone();
            MenuItem::new(id, text)
                .checked(view.groups.order == order)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.state.update(cx, |state, cx| {
                        if state.update_view(&reference, &view_id, |view| view.groups.order = order)
                        {
                            cx.notify();
                        }
                    });
                    cx.notify();
                }))
        };
        div()
            .relative()
            .flex_none()
            .child(
                div()
                    .id(SharedString::from(format!("view-sort-{index}")))
                    .text_color(if open { colors.text } else { colors.text_muted })
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.text))
                    .child(label)
                    .on_mouse_down(MouseButton::Left, |_, window, cx| {
                        window.prevent_default();
                        cx.stop_propagation();
                    })
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        this.menus.toggle(menu, down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |trigger| {
                trigger.child(
                    Popover::new(
                        Menu::new("group-order-menu")
                            .label("order the groups")
                            .item(item("order-worst", "worst first", GroupOrder::WorstFirst))
                            .item(item("order-name", "by name", GroupOrder::Name))
                            .on_dismiss(Self::dismiss_listener(cx)),
                    )
                    .align_right(),
                )
            })
            .into_any_element()
    }

    /// A view header's `···`: edit the dashboard, a list's grouping and
    /// handled toggle, collapse, copy the filter.
    fn view_options_trigger(
        &self,
        reference: &DashboardRef,
        view: &View,
        index: usize,
        collapsed: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let menu = HeaderMenu::ViewOptions(index);
        let open = self.menus.open() == Some(menu);
        let trigger = GlyphButton::new(SharedString::from(format!("view-options-{index}")), "···")
            .text_size(px(13.))
            .bleed()
            .color(theme.colors.text_muted)
            .selected(open)
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                cx.stop_propagation();
                this.menus.toggle(menu, down_position(event));
                cx.notify();
            }));
        let content = open.then(|| {
            let defaults = self.state.read(cx).handled_defaults();
            let fold_id = super::page::Id::from(view.id.as_str());
            let base = if view.is_list() {
                Self::options_menu(reference, view, defaults, cx)
            } else {
                let filter = view.filter.clone();
                let edit = reference.clone();
                let menu = Menu::new("options-menu").item(
                    MenuItem::new("edit-dashboard", "edit dashboard").on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.menus.close();
                            cx.emit(super::DashboardEvent::Edit(edit.clone()));
                            cx.notify();
                        },
                    )),
                );
                // A grid's hosts as squares (the default) or labelled
                // cells (5d), switched in place.
                let menu = if view.display == ic_config::ViewDisplay::HostGroupGrid {
                    use ic_config::GridCells;
                    let cells = |id: &'static str, label: &'static str, choice: GridCells| {
                        let reference = reference.clone();
                        let view_id = view.id.clone();
                        MenuItem::new(id, label)
                            .checked(view.grid.cells == choice)
                            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                                this.menus.close();
                                this.state.update(cx, |state, cx| {
                                    if state.update_view(&reference, &view_id, |view| {
                                        view.grid.cells = choice;
                                    }) {
                                        cx.notify();
                                    }
                                });
                                cx.notify();
                            }))
                    };
                    menu.separator()
                        .label("hosts as")
                        .item(cells("hosts-squares", "squares", GridCells::Squares))
                        .item(cells(
                            "hosts-cells",
                            "labelled cells",
                            GridCells::LabelledCells,
                        ))
                        .separator()
                } else {
                    menu
                };
                menu.item(
                    MenuItem::new("copy-filter", "copy filter expression")
                        .disabled(filter.trim().is_empty())
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.menus.close();
                            cx.write_to_clipboard(ClipboardItem::new_string(filter.clone()));
                            cx.notify();
                        })),
                )
                .on_dismiss(Self::dismiss_listener(cx))
            };
            let reference = reference.clone();
            base.separator().item(
                MenuItem::new(
                    "fold-view",
                    if collapsed {
                        "expand view"
                    } else {
                        "collapse view"
                    },
                )
                .key_hint(if collapsed { "→" } else { "←" })
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.fold_view(&reference, &fold_id, !collapsed, cx);
                })),
            )
        });
        div()
            .relative()
            .flex_none()
            .child(if open {
                trigger
            } else {
                trigger.tooltip(Tooltip::new("View options"))
            })
            .children(content.map(|menu| Popover::new(menu).align_right()))
            .into_any_element()
    }
}

/// The handled slot's width, in characters: `999 handled · hide` fits.
const HANDLED_SLOT_CHARS: usize = 18;

/// The handled slot's width at `theme`'s text size.
fn handled_slot_width(theme: &ic_ui_kit::Theme) -> Pixels {
    #[expect(clippy::cast_precision_loss, reason = "a short slot")]
    let chars = HANDLED_SLOT_CHARS as f32;
    (theme.text.small * (chars * ic_ui_kit::CHAR_WIDTH)).ceil()
}

/// The handled button's words: (`28 hidden`, `show`) while handled
/// problems are hidden, (`28 handled`, `hide`) while they show; `None`
/// when none are handled.
pub(crate) fn handled_text(hidden: u32, handled: u32) -> Option<(String, &'static str)> {
    if hidden > 0 {
        Some((format!("{hidden} hidden"), "show"))
    } else if handled > 0 {
        Some((format!("{handled} handled"), "hide"))
    } else {
        None
    }
}

/// A display's icon (in the view header's mark slot): list, grouped list,
/// grid, tiles, stream.
pub(crate) fn display_icon(display: ic_config::ViewDisplay) -> IconName {
    use ic_config::ViewDisplay;
    match display {
        ViewDisplay::List => IconName::List,
        ViewDisplay::GroupedList => IconName::Rows,
        ViewDisplay::HostGroupGrid => IconName::LayoutGrid,
        ViewDisplay::SummaryTiles => IconName::ChartBar,
        ViewDisplay::EventStream => IconName::Activity,
    }
}

/// What a view header says the view shows, faint: a list's or stream's
/// filter (or what it lists), a grid's or tiles' groups (and a grid's
/// colouring).
pub(crate) fn filter_summary(view: &View) -> String {
    use ic_config::{GridColour, GroupSource, ViewDisplay};
    let groups = || match view.groups.by {
        GroupSource::HostGroup if view.groups.host_groups.is_empty() => {
            "all host groups".to_owned()
        }
        GroupSource::HostGroup => format!("host groups {}", view.groups.host_groups.join(", ")),
        GroupSource::CustomVar => format!("host.vars.{}", view.groups.custom_var_name()),
    };
    let filter = view.filter.trim();
    let with_filter = |text: String| {
        if filter.is_empty() {
            text
        } else {
            format!("{text} · {filter}")
        }
    };
    match view.display {
        ViewDisplay::HostGroupGrid => with_filter(format!(
            "{} · {}",
            groups(),
            match view.grid.colour {
                GridColour::WorstOfHostAndServices => "worst of host and services",
                GridColour::HostOnly => "host only",
            }
        )),
        ViewDisplay::SummaryTiles => with_filter(groups()),
        ViewDisplay::EventStream if filter.is_empty() => "every host and service".to_owned(),
        ViewDisplay::List | ViewDisplay::GroupedList if filter.is_empty() => {
            view_label(view).to_owned()
        }
        _ => filter.to_owned(),
    }
}

/// A view header's counts: the unhandled problems as dots in their
/// colours ([`problem_dots`]); a list without any counts what it lists
/// (OK, pending), as the summary bar does.
fn header_counts(summary: &Summary, kind: ObjectKind, list: bool) -> Vec<(CheckableState, u32)> {
    let problems = problem_dots(summary);
    if !problems.is_empty() || !list {
        return problems;
    }
    summary_items(summary, kind)
        .into_iter()
        .map(|(state, count, _)| (state, count))
        .collect()
}

/// Problem counts as dots, by colour: red (critical services and down
/// hosts), yellow (warning), purple (unknown services and unreachable
/// hosts). A grid colours a host by its services too, so its counts mix
/// both kinds.
pub(crate) fn problem_dots(summary: &Summary) -> Vec<(CheckableState, u32)> {
    [
        (
            CheckableState::Service(ServiceState::Critical),
            summary.critical + summary.down,
        ),
        (
            CheckableState::Service(ServiceState::Warning),
            summary.warning,
        ),
        (
            CheckableState::Service(ServiceState::Unknown),
            summary.unknown + summary.unreachable,
        ),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .collect()
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
pub(crate) fn natural_descending(key: SortKey) -> bool {
    matches!(key, SortKey::Severity | SortKey::LastStateChange)
}

/// What a view lists, as the header's subtitle.
pub(crate) fn view_label(view: &View) -> &'static str {
    match (view.object_kind, view.problems_only) {
        (ObjectKind::Services, true) => "service problems",
        (ObjectKind::Hosts, true) => "host problems",
        (ObjectKind::Services, false) => "services",
        (ObjectKind::Hosts, false) => "hosts",
    }
}

/// The sort trigger's text: `severity ↓`.
pub(crate) fn sort_label(sort: Sort, kind: ObjectKind) -> SharedString {
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
pub(crate) fn summary_items(
    summary: &Summary,
    kind: ObjectKind,
) -> Vec<(CheckableState, u32, &'static str)> {
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

/// The `demo` chip: the environment on screen is one of the demo's own,
/// simulated (ENV-10).
fn demo_chip(theme: &ic_ui_kit::Theme) -> AnyElement {
    div()
        .id("demo-chip")
        .flex_none()
        .px(px(5.))
        .rounded(theme.metrics.small_radius)
        .border_1()
        .border_color(theme.colors.accent.opacity(0.5))
        .text_size(theme.text.hint)
        .text_color(theme.colors.accent_text)
        .child("demo")
        .tooltip(Tooltip::text(
            "A demo environment: simulated by icygui, no real Icinga is involved",
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use gpui::point;

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
    fn the_handled_slot_says_hidden_show_or_handled_hide() {
        assert_eq!(handled_text(28, 30), Some(("28 hidden".to_owned(), "show")));
        assert_eq!(handled_text(0, 28), Some(("28 handled".to_owned(), "hide")));
        assert_eq!(handled_text(0, 0), None, "nothing handled: an empty slot");
    }

    #[test]
    fn view_headers_summarise_what_a_view_shows() {
        use ic_config::{GroupSource, ViewDisplay};
        let mut grid = View {
            display: ViewDisplay::HostGroupGrid,
            ..View::default()
        };
        assert_eq!(
            filter_summary(&grid),
            "all host groups · worst of host and services"
        );
        grid.groups.host_groups = vec!["pg-*".to_owned(), "mysql-*".to_owned()];
        grid.display = ViewDisplay::SummaryTiles;
        assert_eq!(filter_summary(&grid), "host groups pg-*, mysql-*");
        grid.groups.by = GroupSource::CustomVar;
        grid.groups.custom_var = "role".to_owned();
        assert_eq!(filter_summary(&grid), "host.vars.role");
        let list = View {
            filter: "service.problem".to_owned(),
            ..View::default()
        };
        assert_eq!(filter_summary(&list), "service.problem");
        assert_eq!(filter_summary(&View::default()), "service problems");
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
