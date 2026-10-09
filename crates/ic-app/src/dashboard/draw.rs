//! Drawing the page's items: rows, the host-with-services bands and their
//! paging rows, a grid's lines of squares or cells, lines of tiles, an
//! event stream, and the band of the host whose rows are scrolling, which
//! sticks to the top (10k).
//!
//! The page scrolls in one container; only the items on screen are built,
//! between two spacers as tall as the items above and below them, so the
//! scroll bar and the scroll position match the whole page.

use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, AppContext as _, BoxShadow, ClickEvent, Context, ElementId, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, point, prelude::FluentBuilder as _,
    uniform_list,
};
use ic_config::{ObjectKind, View};
use ic_core::LogEntry;
use ic_core::snapshot::{DashboardRow, GridCell, GridGroup, Snapshot, Summary, Tile};
use ic_model::{CheckableState, HostState, ObjectKey, ServiceState, Timestamp};
use ic_rules::DashboardRef;
use ic_ui_kit::{
    ActiveTheme as _, Icon, IconName, ObjectMark, RowEmphasis, Scrollbar, StateDot, Theme, px,
};

use super::page::{
    GridLayout, GridLine, GroupFilter, ItemKind, ListGroup, Page, Stop, StopEntry, ViewPage,
    ViewState,
};
use super::{DashboardView, PageUi, object_row, row_id};
use crate::format;
use crate::notifications::history::{self, HistoryTone};

/// Width of the accent bar on marked rows and bands.
const MARK_WIDTH: f32 = 2.;

impl DashboardView {
    /// The settings of a page view.
    fn config_view(&self, reference: &DashboardRef, page_view: &ViewPage, cx: &App) -> View {
        self.shown_views(reference, cx)
            .and_then(|views| views.get(page_view.index))
            .cloned()
            .unwrap_or_default()
    }

    /// The page: the items on screen between two spacers, in one scrolling
    /// column, with the sticky band and the scroll bar over it.
    pub(super) fn render_page(
        &mut self,
        reference: &DashboardRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(ui) = self.pages.get(reference) else {
            return div().into_any_element();
        };
        let page = ui.page.clone();
        let scroll = ui.scroll.clone();
        // The viewport as last drawn; before the first frame, the window's
        // height (more than the page shows, never less).
        let drawn = scroll.bounds().size.height;
        let viewport = if drawn > px(0.) {
            drawn
        } else {
            window.viewport_size().height
        };
        let max = (page.height() - viewport).max(px(0.));
        let offset = (-scroll.offset().y).clamp(px(0.), max);
        let range = page.items_between(offset, offset + viewport);
        self.visible = range.clone();
        #[cfg(test)]
        self.shown_times.clear();
        let items: Vec<AnyElement> = range
            .clone()
            .map(|index| self.render_item(reference, &page, index, cx))
            .collect();
        let above = page.top(range.start);
        let below = page.height() - page.top(range.end);
        let sticky = self.sticky_band(reference, &page, offset, cx);
        let content = div()
            .flex()
            .flex_col()
            .w_full()
            .flex_none()
            .child(div().flex_none().h(above))
            .children(items)
            .child(div().flex_none().h(below));
        div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .id("dashboard-page")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&scroll)
                    .child(content),
            )
            .children(sticky)
            .child(Scrollbar::vertical(&scroll))
            .into_any_element()
    }

    /// The band of the group whose rows are at the top of the page, when
    /// its own band has scrolled away: it sticks to the top (10k).
    fn sticky_band(
        &mut self,
        reference: &DashboardRef,
        page: &Rc<Page>,
        offset: Pixels,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let top = page.item_at(offset)?;
        let item = page.items[top];
        let (ItemKind::Row {
            group: Some(group), ..
        }
        | ItemKind::More { group }) = item.kind
        else {
            return None;
        };
        let view = page.views.get(item.view)?;
        let band = (view.items.start..top).rev().find(|&index| {
            matches!(page.items[index].kind, ItemKind::Band { group: band } if band == group)
        })?;
        if page.top(band) >= offset {
            return None;
        }
        let element = self.render_band(reference, page, band, true, cx);
        let element = self.picks_view(div().size_full().child(element), page, item.view, cx);
        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(page.item_height(band))
                .shadow(vec![BoxShadow {
                    color: cx.theme().colors.shadow_strong,
                    offset: point(px(0.), px(6.)),
                    blur_radius: px(12.),
                    spread_radius: px(-6.),
                    inset: false,
                }])
                .child(element)
                .into_any_element(),
        )
    }

    /// Item `index` of the page.
    fn render_item(
        &mut self,
        reference: &DashboardRef,
        page: &Rc<Page>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let item = page.items[index];
        let height = page.item_height(index);
        let element = match item.kind {
            ItemKind::Header => self.render_view_header(reference, page, item.view, cx),
            ItemKind::Note => self.render_note(reference, page, item.view, cx),
            ItemKind::Row { row, group } => {
                self.render_row(reference, page, item.view, row, group, cx)
            }
            ItemKind::Band { .. } => self.render_band(reference, page, index, false, cx),
            ItemKind::More { group } => self.render_more(reference, page, item.view, group, cx),
            ItemKind::Grid { line } => self.render_grid_line(reference, page, item.view, line, cx),
            ItemKind::Tiles { line } => self.render_tiles(reference, page, item.view, line, cx),
            ItemKind::Stream => self.render_stream(reference, page, item.view, cx),
            ItemKind::Thread { line } => {
                self.render_thread_line(reference, page, item.view, line, cx)
            }
        };
        // A grid's ringed group (the page's filter) reaches into the space
        // around its line; everything else stays in its item.
        let clip = !matches!(item.kind, ItemKind::Grid { .. });
        let wrapper = div()
            .flex_none()
            .w_full()
            .h(height)
            .when(clip, gpui::Styled::overflow_hidden)
            .child(element);
        self.picks_view(wrapper, page, item.view, cx)
    }

    /// In the editor's preview, a press anywhere in a view picks it (before
    /// the item's own listeners, which may stop it): `element` belongs to
    /// view `view` of `page`.
    fn picks_view(
        &self,
        element: gpui::Div,
        page: &Page,
        view: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let picked = self
            .is_preview()
            .then(|| page.views.get(view).map(|view| view.id.to_string()))
            .flatten();
        match picked {
            Some(id) => element
                .capture_any_mouse_down(cx.listener(move |_, _: &gpui::MouseDownEvent, _, cx| {
                    cx.emit(super::DashboardEvent::Pick(id.clone()));
                }))
                .into_any_element(),
            None => element.into_any_element(),
        }
    }

    /// A view's note in place of its body.
    fn render_note(
        &self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let state = self.state.read(cx);
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let config = self
            .shown_views(reference, cx)
            .and_then(|views| views.get(page_view.index));
        let (text, color) = match page_view.state {
            ViewState::Error => (
                format!(
                    "This view's filter doesn't work: {}",
                    self.shown_result(reference, &page_view.id, cx)
                        .and_then(|result| result.error.clone())
                        .unwrap_or_default()
                ),
                theme.states.text.critical,
            ),
            ViewState::Denied => (
                config
                    .and_then(|view| state.query_denial(view.object_kind))
                    .unwrap_or_default(),
                theme.colors.text_muted,
            ),
            _ => ("Being evaluated…".to_owned(), theme.colors.text_muted),
        };
        div()
            .flex()
            .items_center()
            .size_full()
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(theme.colors.border_row)
            .text_size(theme.text.small)
            .text_color(color)
            .truncate()
            .child(text)
            .into_any_element()
    }

    /// An object row: a standard list row, `service on host`, no indent
    /// (also under a host's band).
    fn render_row(
        &mut self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        row: usize,
        group: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = self.state.read(cx);
        let theme = cx.theme();
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let Some(DashboardRow::Object(key)) = page_view.rows.get(row) else {
            return div().into_any_element();
        };
        let group_id = group
            .and_then(|group| page_view.groups.get(group))
            .map(|group| group.id.clone());
        let stop = Stop::Row {
            view: page_view.id.clone(),
            group: group_id.clone(),
            key: key.clone(),
        };
        let Some(ui) = self.pages.get(reference) else {
            return div().into_any_element();
        };
        let emphasis = RowEmphasis::new(
            ui.selection.cursor_stop() == Some(&stop),
            ui.selection.is_marked(key),
        );
        let multi = page.views.len() > 1 || page.header_stop(0).is_some();
        let id = row_id(multi.then_some(&*page_view.id), group_id.as_deref(), key);
        let times = state.appearance().list_times;
        let row = object_row(
            state.snapshot(),
            id,
            key,
            true,
            times,
            Timestamp::now(),
            theme,
        );
        #[cfg(test)]
        let time = row.time_text().map(ToString::to_string).unwrap_or_default();
        // An action on its way shows instead of the tag.
        let row = match state.pending_label(key) {
            Some(pending) => row.tag(pending),
            None => row,
        };
        let element = row
            .density(crate::controls::ui_density(page_view.density))
            .emphasis(emphasis)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_stop(&stop, event.modifiers(), window, cx);
            }))
            .into_any_element();
        #[cfg(test)]
        self.shown_times.push(time);
        element
    }

    /// A group's band (the host-with-services style, 10h): 36px on
    /// `row_header`, the chevron at the left (it only folds), the host's
    /// state dot in the rows' mark column, the name, the address and status
    /// faint, the per-state counts at the right. A click anywhere but the
    /// chevron opens the host in the pane. Collapsed with marked rows, it
    /// takes the marked tint and bar, so no mark is out of sight.
    #[expect(
        clippy::too_many_lines,
        reason = "one band, its slots in reading order"
    )]
    pub(super) fn render_band(
        &self,
        reference: &DashboardRef,
        page: &Page,
        index: usize,
        sticky: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let item = page.items[index];
        let ItemKind::Band { group } = item.kind else {
            return div().into_any_element();
        };
        let Some(page_view) = page.views.get(item.view) else {
            return div().into_any_element();
        };
        let Some(group) = page_view.groups.get(group) else {
            return div().into_any_element();
        };
        let Some(ui) = self.pages.get(reference) else {
            return div().into_any_element();
        };
        let stop = Stop::Band {
            view: page_view.id.clone(),
            group: group.id.clone(),
        };
        let marked = group.collapsed && ui.selection.any_marked(group_keys(page_view, group));
        let emphasis = RowEmphasis::new(ui.selection.cursor_stop() == Some(&stop), marked);
        let host = group
            .host
            .as_ref()
            .and_then(|name| snapshot.hosts.get(name));
        let now = Timestamp::now();
        let (lead, name, faint, status) = match host {
            Some(host) => (
                StateDot::mark(ObjectMark::host(host)).size(theme.metrics.sidebar_dot),
                if host.display_name.is_empty() {
                    host.name.to_string()
                } else {
                    host.display_name.clone()
                },
                host.address.clone(),
                host_status(host, now),
            ),
            None => (
                StateDot::new(
                    group
                        .counts
                        .worst_unhandled
                        .unwrap_or(CheckableState::Service(ServiceState::Ok)),
                )
                .size(theme.metrics.sidebar_dot),
                group.label.clone(),
                super::rows::count_label(
                    group.members.len(),
                    &self.config_view(reference, page_view, cx),
                ),
                String::new(),
            ),
        };
        let chevron_stop = stop.clone();
        let chevron = div()
            .id(SharedString::from(format!(
                "band-chevron:{}\u{1f}{}{}",
                page_view.id,
                group.id,
                if sticky { ":sticky" } else { "" }
            )))
            .absolute()
            .left(px(14.))
            .top_0()
            .bottom_0()
            .w(px(12.))
            .flex()
            .items_center()
            .cursor_pointer()
            .child(
                Icon::new(if group.collapsed {
                    IconName::ChevronRight
                } else {
                    IconName::ChevronDown
                })
                .size(px(12.))
                .color(colors.text_faint),
            )
            .on_mouse_down(gpui::MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.click_chevron(&chevron_stop, window, cx);
            }));
        let counts = band_counts(&group.counts)
            .into_iter()
            .map(|(state, count)| {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(StateDot::new(state).size(px(7.)))
                    .child(count.to_string())
            });
        let background = emphasis.background(theme).unwrap_or(colors.row_header);
        div()
            .id(SharedString::from(format!(
                "band:{}\u{1f}{}{}",
                page_view.id,
                group.id,
                if sticky { ":sticky" } else { "" }
            )))
            .relative()
            .flex()
            .items_center()
            .gap(px(14.))
            .size_full()
            .px(theme.metrics.list_padding)
            .bg(background)
            .border_b_1()
            .border_color(colors.border_header)
            .whitespace_nowrap()
            .cursor_pointer()
            .when(emphasis == RowEmphasis::None, |band| {
                band.hover(|style| style.bg(colors.row_hover))
            })
            .when(emphasis.is_marked(), |band| {
                band.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(MARK_WIDTH))
                        .bg(colors.accent),
                )
            })
            .child(chevron)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_center()
                    .w(theme.metrics.row_leading)
                    .child(lead),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_baseline()
                    .gap(px(10.))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex_none()
                            .text_size(theme.text.row)
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(colors.text_emphasis)
                            .child(name),
                    )
                    .when(!faint.is_empty(), |text| {
                        text.child(
                            div()
                                .flex_none()
                                .text_size(theme.text.small)
                                .text_color(colors.text_faint)
                                .child(faint),
                        )
                    })
                    .when(!status.is_empty(), |text| {
                        text.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_faint)
                                .child(status),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(12.))
                    .text_size(theme.text.small)
                    .text_color(colors.text_muted)
                    .children(counts),
            )
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_stop(&stop, event.modifiers(), window, cx);
            }))
            .into_any_element()
    }

    /// A host's paging row: `+ 12 more`, or `− show fewer` in the same
    /// slot, in the rows' text column. It takes the marked tint while it
    /// hides marked rows.
    fn render_more(
        &self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        group: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let Some(group) = page_view.groups.get(group) else {
            return div().into_any_element();
        };
        let Some(ui) = self.pages.get(reference) else {
            return div().into_any_element();
        };
        let stop = Stop::More {
            view: page_view.id.clone(),
            group: group.id.clone(),
        };
        let hidden_marked = !group.expanded
            && ui
                .selection
                .any_marked(group.members[group.shown..].iter().filter_map(|&row| {
                    match &page_view.rows[row] {
                        DashboardRow::Object(key) => Some(key),
                        DashboardRow::Group { .. } => None,
                    }
                }));
        let emphasis = RowEmphasis::new(ui.selection.cursor_stop() == Some(&stop), hidden_marked);
        div()
            .id(SharedString::from(format!(
                "more:{}\u{1f}{}",
                page_view.id, group.id
            )))
            .relative()
            .flex()
            .items_center()
            .gap(px(14.))
            .size_full()
            .px(theme.metrics.list_padding)
            .border_b_1()
            .border_color(colors.border_row)
            .text_size(theme.text.small)
            .text_color(colors.text_faint)
            .cursor_pointer()
            .when_some(emphasis.background(theme), gpui::Styled::bg)
            .when(emphasis == RowEmphasis::None, |row| {
                row.hover(|style| style.bg(colors.row_hover).text_color(colors.text_muted))
            })
            .when(emphasis.is_marked(), |row| {
                row.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(MARK_WIDTH))
                        .bg(colors.accent),
                )
            })
            .child(div().flex_none().w(theme.metrics.row_leading))
            .child(crate::paging::more_label(group.hidden()))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_stop(&stop, event.modifiers(), window, cx);
            }))
            .into_any_element()
    }

    /// A line of a host-group grid (topic 05).
    fn render_grid_line(
        &self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        line: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let Some(layout) = page_view.grid.as_ref() else {
            return div().into_any_element();
        };
        let Some(ui) = self.pages.get(reference) else {
            return div().into_any_element();
        };
        let sizes = super::page::Sizes::of(theme);
        let first = line == 0;
        match &layout.lines[line] {
            GridLine::Groups(range) => {
                let blocks = range.clone().map(|group| {
                    self.grid_block(reference, ui, page_view, layout, group, true, cx)
                        .into_any_element()
                });
                let fillers = (range.len()..layout.columns)
                    .map(|_| div().flex_1().min_w_0().into_any_element());
                div()
                    .flex()
                    .items_start()
                    .gap(sizes.column_gap)
                    .size_full()
                    .px(theme.metrics.list_padding)
                    .pt(if first {
                        sizes.body_top
                    } else {
                        sizes.line_gap
                    })
                    .children(blocks)
                    .children(fillers)
                    .into_any_element()
            }
            GridLine::Header(group) => div()
                .flex()
                .flex_col()
                .size_full()
                .px(theme.metrics.list_padding)
                .pt(if first {
                    sizes.body_top
                } else {
                    sizes.section_gap
                })
                .child(self.grid_block(reference, ui, page_view, layout, *group, false, cx))
                .into_any_element(),
            GridLine::Cells { group, cells } => {
                let entry = &layout.grid.groups[*group];
                let dim = layout.on.is_some_and(|on| on != *group);
                let elements = cells.clone().map(|cell| {
                    self.grid_cell(ui, page_view, entry, cell, cx)
                        .into_any_element()
                });
                let fillers = (cells.len()..layout.per_line)
                    .map(|_| div().flex_1().min_w_0().into_any_element());
                div()
                    .flex()
                    .gap(sizes.cell_gap)
                    .w_full()
                    .h(sizes.cell)
                    .px(theme.metrics.list_padding)
                    .when(dim, |line| line.opacity(0.35))
                    .children(elements)
                    .children(fillers)
                    .into_any_element()
            }
        }
    }

    /// A grid group: its header (dot, name that filters the page, host
    /// count, problem counts) and, with squares, its squares.
    #[expect(
        clippy::too_many_arguments,
        reason = "the parts of a grid a block draws"
    )]
    fn grid_block(
        &self,
        reference: &DashboardRef,
        ui: &PageUi,
        page_view: &ViewPage,
        layout: &GridLayout,
        group: usize,
        squares: bool,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let colors = theme.colors;
        let sizes = super::page::Sizes::of(theme);
        let entry = &layout.grid.groups[group];
        let on = layout.on == Some(group);
        let dim = layout.on.is_some() && !on;
        let header = self.grid_group_header(reference, page_view, entry, cx);
        let block = div()
            .id(SharedString::from(format!(
                "grid-group:{}\u{1f}{}",
                page_view.id, entry.name
            )))
            .flex()
            .flex_col()
            .gap(sizes.group_gap)
            .flex_1()
            .min_w_0()
            .rounded(px(6.))
            .when(dim, |block| block.opacity(0.35))
            .when(on, |block| {
                block
                    .border_1()
                    .border_color(colors.accent)
                    .mx(px(-8.))
                    .mt(px(-6.))
                    .mb(px(-8.))
                    .px(px(7.))
                    .pt(px(5.))
                    .pb(px(7.))
            })
            .child(header);
        if !squares {
            return block;
        }
        let cells = entry.cells.iter().enumerate().map(|(index, cell)| {
            self.grid_square(ui, page_view, entry, index, cell, cx)
                .into_any_element()
        });
        block.child(
            div()
                .flex()
                .flex_wrap()
                .gap(sizes.square_gap)
                .children(cells),
        )
    }

    /// A grid group's header line. A click on the name filters the whole
    /// page to the group (again: back to all).
    fn grid_group_header(
        &self,
        reference: &DashboardRef,
        page_view: &ViewPage,
        entry: &GridGroup,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let view = self.config_view(reference, page_view, cx);
        let filter = GroupFilter {
            by: view.groups.by,
            var: view.groups.custom_var_name().to_owned(),
            name: entry.name.clone(),
            label: entry.label.clone(),
        };
        let reference = reference.clone();
        let worst = entry
            .counts
            .worst_unhandled
            .unwrap_or(CheckableState::Host(HostState::Up));
        let counts =
            super::header::problem_dots(&entry.counts)
                .into_iter()
                .map(|(state, count)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .child(StateDot::new(state).size(px(6.)))
                        .child(count.to_string())
                });
        div()
            .flex()
            .items_center()
            .gap(px(9.))
            .h(px(18.))
            .whitespace_nowrap()
            .text_size(theme.text.body)
            .child(StateDot::new(worst).size(theme.metrics.sidebar_dot))
            .child(
                div()
                    .id(SharedString::from(format!(
                        "grid-group-name:{}\u{1f}{}",
                        page_view.id, entry.name
                    )))
                    .flex_none()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .cursor_pointer()
                    .hover(|style| style.text_color(colors.accent_text))
                    .child(entry.label.clone())
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.filter_to(&reference, filter.clone(), cx);
                    })),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(entry.cells.len().to_string()),
            )
            .child(
                div()
                    .ml_auto()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .text_size(theme.text.label)
                    .text_color(colors.text_muted)
                    .children(counts),
            )
            .into_any_element()
    }

    /// A host's square: healthy dim green, problems full colour, handled
    /// (acknowledged, or in downtime whatever the state) a hollow ring;
    /// the cursor's has an accent outline. Hover shows the host; a click
    /// opens it.
    fn grid_square(
        &self,
        ui: &PageUi,
        page_view: &ViewPage,
        entry: &GridGroup,
        index: usize,
        cell: &GridCell,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let colors = theme.colors;
        let stop = Stop::Cell {
            view: page_view.id.clone(),
            group: super::page::Id::from(entry.name.as_str()),
            host: cell.host.clone(),
        };
        let cursor = ui.selection.cursor_stop() == Some(&stop);
        let fill = theme.states.fill.checkable(cell.state);
        let healthy = is_ok(cell.state) && !cell.handled;
        // Built on hover only: a grid draws thousands of squares.
        let state = self.state.clone();
        let hovered = cell.clone();
        div()
            // Unique inside its group's block, which has the group's id.
            .id(ElementId::NamedInteger("square".into(), index as u64))
            .relative()
            .flex_none()
            .size(px(12.))
            .rounded(px(2.))
            .cursor_pointer()
            .map(|square| {
                if cell.handled {
                    square.border_2().border_color(fill)
                } else if healthy {
                    square.bg(fill.opacity(dim_green(theme)))
                } else {
                    square.bg(fill)
                }
            })
            .when(cursor, |square| {
                square.child(
                    div()
                        .absolute()
                        .top(px(-3.))
                        .left(px(-3.))
                        .size(px(18.))
                        .rounded(px(4.))
                        .border_2()
                        .border_color(colors.accent),
                )
            })
            .tooltip(move |_, cx| {
                let tooltip = HostTooltip::of(state.read(cx).snapshot(), &hovered);
                tooltip.view(cx)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_stop(&stop, event.modifiers(), window, cx);
            }))
    }

    /// A labelled cell (`hosts as: labelled cells`, 5d): the host's dot and
    /// name; a problem's cell is filled and names its worst service.
    fn grid_cell(
        &self,
        ui: &PageUi,
        page_view: &ViewPage,
        entry: &GridGroup,
        index: usize,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let colors = theme.colors;
        let cell = &entry.cells[index];
        let stop = Stop::Cell {
            view: page_view.id.clone(),
            group: super::page::Id::from(entry.name.as_str()),
            host: cell.host.clone(),
        };
        let cursor = ui.selection.cursor_stop() == Some(&stop);
        let state = self.state.read(cx);
        let note = cell_note(state.snapshot(), cell);
        let problem = !is_ok(cell.state);
        let app_state = self.state.clone();
        let hovered = cell.clone();
        div()
            .id(SharedString::from(format!(
                "cell:{}\u{1f}{}\u{1f}{}",
                page_view.id, entry.name, cell.host
            )))
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(8.))
            .h_full()
            .px(px(8.))
            .rounded(px(4.))
            .whitespace_nowrap()
            .overflow_hidden()
            .text_size(theme.text.small)
            .cursor_pointer()
            // Every cell has the same 1px border (transparent but on OK
            // cells), so cells share their line's width equally and a
            // change of state never moves a column edge.
            .border_1()
            .map(|cell_box| {
                if cursor {
                    cell_box
                        .bg(colors.row_selected)
                        .border_color(gpui::transparent_black())
                } else if problem {
                    cell_box
                        .bg(colors.element_background)
                        .border_color(gpui::transparent_black())
                } else {
                    cell_box.border_color(colors.border_row)
                }
            })
            .child(
                StateDot::new(cell.state)
                    .hollow(cell.handled)
                    .size(if problem { px(7.) } else { px(6.) }),
            )
            .child(
                div()
                    .flex_none()
                    .text_color(if problem {
                        colors.text_strong
                    } else {
                        colors.text_muted
                    })
                    .child(cell.host.to_string()),
            )
            .when_some(note, |cell_box, note| {
                cell_box.child(
                    div()
                        .ml_auto()
                        .pl(px(6.))
                        .min_w_0()
                        .truncate()
                        .text_size(theme.text.hint)
                        .text_color(colors.text_faint)
                        .child(note),
                )
            })
            .tooltip(move |_, cx| {
                let tooltip = HostTooltip::of(app_state.read(cx).snapshot(), &hovered);
                tooltip.view(cx)
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_stop(&stop, event.modifiers(), window, cx);
            }))
    }

    /// A line of summary tiles (topic 04).
    fn render_tiles(
        &self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        line: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let Some(layout) = page_view.tiles.as_ref() else {
            return div().into_any_element();
        };
        let sizes = super::page::Sizes::of(theme);
        let start = line * layout.columns;
        let end = (start + layout.columns).min(layout.tiles.len());
        let config = self.config_view(reference, page_view, cx);
        let tiles = (start..end).map(|index| {
            let dim = layout.on.is_some_and(|on| on != index);
            Self::tile(
                reference,
                &config,
                page_view,
                &layout.tiles[index],
                layout.on == Some(index),
                dim,
                cx,
            )
            .into_any_element()
        });
        let fillers =
            (end - start..layout.columns).map(|_| div().flex_1().min_w_0().into_any_element());
        div()
            .flex()
            .items_start()
            .gap(sizes.tile_gap)
            .size_full()
            .px(theme.metrics.list_padding)
            .pt(if line == 0 { sizes.body_top } else { px(0.) })
            .children(tiles)
            .children(fillers)
            .into_any_element()
    }

    /// One tile: the worst state's dot, the name, the host count, a stacked
    /// bar and the counts in state colours. A click filters the page to its
    /// group (again: back to all).
    #[expect(
        clippy::too_many_lines,
        reason = "the parts of a tile, drawn in reading order"
    )]
    fn tile(
        reference: &DashboardRef,
        view: &View,
        page_view: &ViewPage,
        tile: &Tile,
        on: bool,
        dim: bool,
        cx: &Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let theme = cx.theme();
        let colors = theme.colors;
        let sizes = super::page::Sizes::of(theme);
        let kind = view.object_kind;
        // Unhandled counts: the tiles add up to the view header's.
        let parts = tile_parts(&tile.counts, kind);
        let total: u32 = parts.iter().map(|(_, count, _)| *count).sum();
        let worst = tile.counts.worst_unhandled.unwrap_or(if total == 0 {
            CheckableState::Service(ServiceState::Pending)
        } else {
            CheckableState::Service(ServiceState::Ok)
        });
        let filter = GroupFilter {
            by: view.groups.by,
            var: view.groups.custom_var_name().to_owned(),
            name: tile.name.clone(),
            label: tile.label.clone(),
        };
        let reference = reference.clone();
        let segments: Vec<(CheckableState, u32)> = parts
            .iter()
            .filter(|(_, count, _)| *count > 0)
            .map(|(state, count, _)| (*state, *count))
            .collect();
        let last = segments.len().saturating_sub(1);
        // GPUI doesn't clip children to a radius: the outer segments round
        // their own ends.
        let bar = div()
            .flex()
            .gap(px(2.))
            .h(px(6.))
            .w_full()
            .rounded(px(3.))
            .bg(colors.border_header)
            .children(segments.iter().enumerate().map(|(index, (state, count))| {
                #[expect(
                    clippy::cast_precision_loss,
                    reason = "counts far below f32's exact range"
                )]
                let share = *count as f32 / total.max(1) as f32;
                div()
                    .h_full()
                    .flex_basis(gpui::relative(share))
                    .bg(theme.states.fill.checkable(*state))
                    .when(index == 0, |segment| segment.rounded_l(px(3.)))
                    .when(index == last, |segment| segment.rounded_r(px(3.)))
            }));
        let numbers =
            parts
                .iter()
                .filter(|(_, count, _)| *count > 0)
                .map(|(state, count, word)| {
                    div()
                        .flex()
                        .flex_none()
                        .gap(px(5.))
                        .child(
                            div()
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(theme.states.text.checkable(*state))
                                .child(count.to_string()),
                        )
                        .child(*word)
                });
        div()
            .id(SharedString::from(format!(
                "tile:{}\u{1f}{}",
                page_view.id, tile.name
            )))
            .flex()
            .flex_col()
            .gap(px(9.))
            .flex_1()
            .min_w_0()
            .h(sizes.tile)
            .px(px(14.))
            .pt(px(11.))
            .pb(px(12.))
            .rounded(px(6.))
            .border_1()
            .border_color(if on {
                colors.accent
            } else {
                colors.border_header
            })
            .bg(colors.window_background)
            .cursor_pointer()
            .when(dim, |tile| tile.opacity(0.35))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .h(px(18.))
                    .whitespace_nowrap()
                    .child(StateDot::new(worst).size(theme.metrics.sidebar_dot))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text.row)
                            .font_weight(gpui::FontWeight::MEDIUM)
                            .text_color(colors.text_strong)
                            .child(tile.label.clone()),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .flex_none()
                            .text_size(theme.text.label)
                            .text_color(colors.text_faint)
                            .child(format!(
                                "{} {}",
                                tile.hosts,
                                if tile.hosts == 1 { "host" } else { "hosts" }
                            )),
                    ),
            )
            .child(bar)
            .child(
                div()
                    .flex()
                    .gap(px(14.))
                    .h(px(16.))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_size(theme.text.small)
                    .text_color(colors.text_muted)
                    .children(numbers),
            )
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.filter_to(&reference, filter.clone(), cx);
            }))
    }

    /// An event stream's lines: `lines` of them show, the rest scroll.
    fn render_stream(
        &mut self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let id = page_view.id.clone();
        let count = page_view.events.len();
        let Some(ui) = self.pages.get_mut(reference) else {
            return div().into_any_element();
        };
        let handle = ui.streams.entry(id.clone()).or_default().clone();
        let reference = reference.clone();
        let list = uniform_list(
            SharedString::from(format!("stream:{id}")),
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                this.render_events(&reference, &id, range, cx)
            }),
        )
        .track_scroll(&handle)
        .size_full();
        div()
            .relative()
            .size_full()
            .child(list)
            .child(Scrollbar::vertical(&handle))
            .into_any_element()
    }

    /// Events `range` of a stream: time, dot, KIND in the state's colour
    /// (acknowledgements and downtimes in the accent), `service on host`,
    /// and the note under it.
    fn render_events(
        &self,
        reference: &DashboardRef,
        view: &super::page::Id,
        range: std::ops::Range<usize>,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(ui) = self.pages.get(reference) else {
            return Vec::new();
        };
        let page = ui.page.clone();
        let Some(page_view) = page.view_by_id(view) else {
            return Vec::new();
        };
        // The view's rows at its own density.
        let theme = &crate::controls::theme_for(cx.theme(), page_view.density);
        let now = Timestamp::now();
        let sizes = super::page::Sizes::of(theme);
        let compact = theme.density == ic_ui_kit::Density::Compact;
        range
            .filter_map(|index| {
                let event = page_view.events.get(index)?;
                let stop = page
                    .stops()
                    .get(page_view.event_stops + index)?
                    .stop
                    .clone();
                let selected = ui.selection.cursor_stop() == Some(&stop);
                Some(
                    event_line(event, index, selected, compact, sizes.event, now, theme)
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_stop(&stop, event.modifiers(), window, cx);
                        }))
                        .into_any_element(),
                )
            })
            .collect()
    }
}

impl DashboardView {
    /// Line `line` of a handling or downtimes view stacked on the page
    /// (drawn as on its own page, [`crate::lists::view::thread_line`]): a
    /// band's chevron folds it, a click elsewhere on a line with a key puts
    /// the cursor there (and opens its object's pane). On a timeline, the
    /// line's piece of the *now* line.
    #[expect(
        clippy::too_many_lines,
        reason = "one line, its slots in reading order"
    )]
    fn render_thread_line(
        &self,
        reference: &DashboardRef,
        page: &Page,
        view: usize,
        line: usize,
        cx: &Context<Self>,
    ) -> AnyElement {
        use crate::lists::model::Mode;
        use crate::lists::threads::{ItemKey, Line};
        use crate::lists::view::{self as lists, BandLine, ThreadLineInput};
        let line_index = line;
        let Some(page_view) = page.views.get(view) else {
            return div().into_any_element();
        };
        let Some(thread) = page_view.thread.as_ref() else {
            return div().into_any_element();
        };
        let Some(keyed) = thread.listing.lines.get(line) else {
            return div().into_any_element();
        };
        let state = self.state.read(cx);
        let snapshot = state.snapshot();
        let theme = crate::controls::theme_for(cx.theme(), page_view.density);
        let now = Timestamp::now();
        let timeline = thread.mode == Mode::Timeline;
        let axis = timeline.then(|| crate::lists::words::axis(now));
        let axis_width = crate::lists::draw::axis_width(self.width, &theme);
        let stop = keyed.key.clone().map(|key| Stop::Thread {
            view: page_view.id.clone(),
            key,
        });
        let cursor = self
            .pages
            .get(reference)
            .and_then(|ui| ui.selection.cursor_stop());
        let emphasis = match &stop {
            Some(stop) => RowEmphasis::new(cursor == Some(stop), false),
            None => RowEmphasis::None,
        };
        // Topic 17: the open comment field, and the comments on their way
        // or refused.
        match &keyed.line {
            Line::Composer { object } => {
                return match &self.composer {
                    Some(open)
                        if open.dashboard == *reference
                            && open.view == page_view.id
                            && open.composer.object == *object =>
                    {
                        crate::comments::lines::composer_line(
                            state.author(),
                            &open.composer,
                            cx.entity_id(),
                            &theme,
                        )
                    }
                    _ => div().into_any_element(),
                };
            }
            Line::Draft { id, .. } => {
                return match state.comment_draft(*id) {
                    Some(draft) => lists::draft_element(&self.state, draft, now, &theme),
                    None => div().into_any_element(),
                };
            }
            _ => {}
        }
        let element = match &keyed.line {
            Line::Band {
                object,
                slot,
                collapsed,
                covers,
            } => {
                let chevron = Stop::Thread {
                    view: page_view.id.clone(),
                    key: ItemKey::Band(object.clone()),
                };
                let band = lists::thread_band(
                    &BandLine {
                        object,
                        slot,
                        collapsed: *collapsed,
                        covers: *covers,
                        id_suffix: "",
                        emphasis,
                        timeline,
                        now,
                    },
                    snapshot,
                    &theme,
                    cx.listener(move |this, _: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        this.click_chevron(&chevron, window, cx);
                    }),
                );
                match stop.clone() {
                    Some(stop) => band
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_stop(&stop, event.modifiers(), window, cx);
                        }))
                        .into_any_element(),
                    None => band.into_any_element(),
                }
            }
            line => {
                let pending = line
                    .entry()
                    .and_then(|entry| lists::entry_pending(state, entry));
                // A thread's last entry offers *+ comment* on hover.
                let plus = (thread.kind == crate::lists::model::ListKind::Handling
                    && self.may_comment(cx)
                    && thread.listing.is_last_entry(line_index))
                .then(|| line.object().cloned())
                .flatten()
                .map(|object| {
                    let (reference, view) = (reference.clone(), page_view.id.clone());
                    crate::comments::lines::plus_comment(
                        ElementId::Name(
                            format!("plus-comment:{view}:{}", object.full_name()).into(),
                        ),
                        &theme,
                        cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.open_stacked_composer(
                                &reference,
                                &view,
                                object.clone(),
                                window,
                                cx,
                            );
                        }),
                    )
                });
                let element = lists::thread_line(
                    &ThreadLineInput {
                        snapshot,
                        listing: &thread.listing,
                        sort: thread.options.sort(thread.kind),
                        axis: axis.as_ref(),
                        axis_width,
                        now,
                        theme: &theme,
                    },
                    line,
                    emphasis,
                    pending,
                    plus,
                );
                match stop.clone() {
                    Some(stop) => div()
                        .id(ElementId::Name(
                            format!(
                                "thread:{}:{}",
                                page_view.id,
                                keyed.key.as_ref().map(lists::key_id).unwrap_or_default()
                            )
                            .into(),
                        ))
                        .size_full()
                        .cursor_pointer()
                        .child(element)
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            this.click_stop(&stop, event.modifiers(), window, cx);
                        }))
                        .into_any_element(),
                    None => element,
                }
            }
        };
        // The timeline's *now*, a piece on each of its lines (but the
        // sections' headings).
        let now_line = axis
            .as_ref()
            .filter(|axis| (0. ..=1.).contains(&axis.now))
            .filter(|_| !matches!(keyed.line, Line::Section { .. }))
            .map(|axis| {
                let left = crate::lists::draw::axis_left(self.width, axis_width, &theme)
                    + axis_width * axis.now;
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(left)
                    .w(ic_ui_kit::Metrics::RULE)
                    .bg(theme.colors.accent)
                    .opacity(0.6)
            });
        div()
            .relative()
            .size_full()
            .child(element)
            .children(now_line)
            .into_any_element()
    }
}

/// The keys of a group's rows.
fn group_keys<'a>(
    page_view: &'a ViewPage,
    group: &'a ListGroup,
) -> impl Iterator<Item = &'a ObjectKey> {
    group
        .members
        .iter()
        .filter_map(|&row| match &page_view.rows[row] {
            DashboardRow::Object(key) => Some(key),
            DashboardRow::Group { .. } => None,
        })
}

/// A host's status on its band: `up 41d · PING OK - rta 0.38ms`.
fn host_status(host: &ic_model::Host, now: Timestamp) -> String {
    let state = format::state_word(CheckableState::Host(host.state));
    let since = format::time_in_state(&host.check, now);
    let output = host
        .check
        .output()
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    match (since.is_empty(), output.is_empty()) {
        (true, true) => state.to_owned(),
        (false, true) => format!("{state} {since}"),
        (true, false) => format!("{state} · {output}"),
        (false, false) => format!("{state} {since} · {output}"),
    }
}

/// A band's counts: its rows by state (handled problems left out), the
/// problems first, then pending and OK.
fn band_counts(summary: &Summary) -> Vec<(CheckableState, u32)> {
    [
        (
            CheckableState::Service(ServiceState::Critical),
            summary.critical,
        ),
        (CheckableState::Host(HostState::Down), summary.down),
        (
            CheckableState::Host(HostState::Unreachable),
            summary.unreachable,
        ),
        (
            CheckableState::Service(ServiceState::Warning),
            summary.warning,
        ),
        (
            CheckableState::Service(ServiceState::Unknown),
            summary.unknown,
        ),
        (
            CheckableState::Service(ServiceState::Pending),
            summary.pending,
        ),
        (CheckableState::Service(ServiceState::Ok), summary.ok),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .collect()
}

/// A tile's parts: each state with its count and word, worst first, OK
/// last.
fn tile_parts(summary: &Summary, kind: ObjectKind) -> Vec<(CheckableState, u32, &'static str)> {
    match kind {
        ObjectKind::Services => vec![
            (
                CheckableState::Service(ServiceState::Critical),
                summary.critical,
                "critical",
            ),
            (
                CheckableState::Service(ServiceState::Unknown),
                summary.unknown,
                "unknown",
            ),
            (
                CheckableState::Service(ServiceState::Warning),
                summary.warning,
                "warning",
            ),
            (
                CheckableState::Service(ServiceState::Pending),
                summary.pending,
                "pending",
            ),
            (CheckableState::Service(ServiceState::Ok), summary.ok, "ok"),
        ],
        ObjectKind::Hosts => vec![
            (CheckableState::Host(HostState::Down), summary.down, "down"),
            (
                CheckableState::Host(HostState::Unreachable),
                summary.unreachable,
                "unreachable",
            ),
            (
                CheckableState::Host(HostState::Pending),
                summary.pending,
                "pending",
            ),
            (CheckableState::Host(HostState::Up), summary.ok, "up"),
        ],
    }
}

/// Whether `state` is OK or UP.
fn is_ok(state: CheckableState) -> bool {
    matches!(
        state,
        CheckableState::Service(ServiceState::Ok) | CheckableState::Host(HostState::Up)
    )
}

/// How strong a healthy host's square is: dim, so the problems stand out
/// (32 % on dark, 38 % on light).
fn dim_green(theme: &Theme) -> f32 {
    if theme.mode == ic_ui_kit::ThemeMode::Light {
        0.38
    } else {
        0.32
    }
}

/// What a labelled cell names at its right, as its mark says it: a filled
/// (unhandled) cell what gives it its state (`host down`, else its worst
/// service); a hollow (handled) one why (`downtime` or `acknowledged`,
/// of the problem it shows, else of the host); an OK cell nothing.
fn cell_note(snapshot: &Snapshot, cell: &GridCell) -> Option<String> {
    let host = snapshot.hosts.get(&cell.host);
    let host_problem = host.filter(|host| cell.state == CheckableState::Host(host.state));
    let service = cell.worst_service.as_ref().and_then(|name| {
        snapshot
            .services
            .get(&ic_model::ServiceKey::new(cell.host.as_str(), name))
    });
    if cell.handled {
        let in_downtime = match (host_problem, service) {
            (None, Some(service)) if service.check.in_downtime() => true,
            (None, Some(service)) if service.check.acknowledgement.is_acknowledged() => false,
            _ => host.is_some_and(|host| host.check.in_downtime()),
        };
        return Some(
            if in_downtime {
                "downtime"
            } else {
                "acknowledged"
            }
            .to_owned(),
        );
    }
    if let Some(host) = host_problem.filter(|host| host.is_problem()) {
        return Some(format!(
            "host {}",
            format::state_word(CheckableState::Host(host.state))
        ));
    }
    cell.worst_service.as_ref().map(ToString::to_string)
}

/// A stream line.
fn event_line(
    event: &LogEntry,
    index: usize,
    selected: bool,
    compact: bool,
    height: Pixels,
    now: Timestamp,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let colors = theme.colors;
    let line = history::line(event, &event.object, now);
    let (dot, word): (Hsla, Hsla) = match line.tone {
        HistoryTone::State(state) => (
            theme.states.fill.checkable(state),
            theme.states.text.checkable(state),
        ),
        HistoryTone::Accent | HistoryTone::Downtime => (colors.accent, colors.accent_text),
        HistoryTone::Flapping => (theme.states.fill.warning, theme.states.text.warning),
        HistoryTone::Quiet => (colors.text_faint, colors.text_faint),
    };
    let (name, host) = match &event.object {
        ObjectKey::Service { key } => (key.name.to_string(), Some(key.host.to_string())),
        ObjectKey::Host { name } => (name.to_string(), None),
    };
    div()
        .id(ElementId::NamedInteger(
            "event".into(),
            u64::try_from(index).unwrap_or(u64::MAX),
        ))
        .flex()
        .items_start()
        .gap(px(10.))
        .w_full()
        .h(height)
        .px(theme.metrics.list_padding)
        .pt(px(6.))
        .border_b_1()
        .border_color(colors.border_row)
        .cursor_pointer()
        .when(selected, |line| line.bg(colors.row_selected))
        .when(!selected, |line| {
            line.hover(|style| style.bg(colors.row_hover))
        })
        .child(
            div()
                .w(px(44.))
                .flex_none()
                .pt(px(2.))
                .text_size(theme.text.hint)
                .text_color(colors.text_faint)
                .child(line.time.clone()),
        )
        .child(
            div()
                .flex_none()
                .pt(px(5.))
                .child(StateDot::with_color(dot).size(px(7.))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .min_w_0()
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_size(theme.text.body)
                        .child(
                            div()
                                .flex_none()
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(word)
                                .child(line.kind),
                        )
                        .child(div().flex_none().text_color(colors.text_strong).child(name))
                        .when_some(host, |title, host| {
                            title
                                .child(div().flex_none().text_color(colors.text_faint).child("on"))
                                .child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(colors.text_secondary)
                                        .child(host),
                                )
                        }),
                )
                .when(!compact && !line.note.is_empty(), |column| {
                    column.child(
                        div()
                            .truncate()
                            .text_size(theme.text.small)
                            .text_color(colors.text_faint)
                            .child(line.note.clone()),
                    )
                }),
        )
}

/// Where a grid host's square (or cell) is on the page, for scrolling it
/// into view; `None` for other stops.
pub(super) fn cell_extent(page: &Page, entry: &StopEntry) -> Option<(Pixels, Pixels)> {
    let Stop::Cell { view, .. } = &entry.stop else {
        return None;
    };
    let layout = page.view_by_id(view)?.grid.as_ref()?;
    let item_top = page.top(entry.item);
    let ItemKind::Grid { line } = page.items.get(entry.item)?.kind else {
        return None;
    };
    let sizes = page.sizes();
    match layout.lines.get(line)? {
        GridLine::Groups(_) => {
            let pad = if line == 0 {
                sizes.body_top
            } else {
                sizes.line_gap
            };
            #[expect(clippy::cast_precision_loss, reason = "a few hundred lines")]
            let row = (entry.sub / layout.per_line.max(1)) as f32;
            let top = item_top
                + pad
                + sizes.group_header
                + sizes.group_gap
                + (sizes.square + sizes.square_gap) * row;
            Some((top, top + sizes.square))
        }
        GridLine::Header(_) | GridLine::Cells { .. } => {
            Some((item_top, item_top + page.item_height(entry.item)))
        }
    }
}

/// The tooltip of a grid host: its dot, name, state and problem count, and
/// its worst service with its output's first line.
#[derive(Clone, Debug)]
struct HostTooltip {
    state: CheckableState,
    handled: bool,
    host: String,
    summary: String,
    detail: Option<String>,
}

impl HostTooltip {
    fn of(snapshot: &Snapshot, cell: &GridCell) -> Self {
        let host = snapshot.hosts.get(&cell.host);
        let host_state = host.map_or(CheckableState::Host(HostState::Pending), |host| {
            CheckableState::Host(host.state)
        });
        let problems = match cell.problems {
            0 => String::new(),
            1 => " · 1 problem".to_owned(),
            count => format!(" · {count} problems"),
        };
        let detail = cell.worst_service.as_ref().map(|service| {
            let key = ic_model::ServiceKey::new(cell.host.as_str(), service);
            let output = snapshot
                .services
                .get(&key)
                .map(|service| {
                    service
                        .check
                        .output()
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
                .unwrap_or_default();
            if output.is_empty() {
                service.to_string()
            } else {
                format!("{service}: {output}")
            }
        });
        Self {
            state: cell.state,
            handled: cell.handled,
            host: cell.host.to_string(),
            summary: format!("{}{problems}", format::state_word(host_state)),
            detail,
        }
    }

    fn view(self, cx: &mut App) -> AnyView {
        cx.new(|_| self).into()
    }
}

impl Render for HostTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let card = div()
            .flex()
            .flex_col()
            .gap(px(3.))
            .px(px(9.))
            .py(px(6.))
            .rounded(theme.metrics.small_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow(vec![BoxShadow {
                color: colors.shadow,
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            }])
            .font_family(theme.font_family.clone())
            .text_size(theme.text.label)
            .whitespace_nowrap()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(StateDot::new(self.state).hollow(self.handled).size(px(6.)))
                    .child(
                        div()
                            .text_color(colors.text_strong)
                            .child(self.host.clone()),
                    )
                    .child(
                        div()
                            .text_color(colors.text_faint)
                            .child(self.summary.clone()),
                    ),
            )
            .when_some(self.detail.clone(), |card, detail| {
                card.child(div().text_color(colors.text_muted).child(detail))
            });
        div().pt(px(18.)).pl(px(4.)).child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_counts_list_problems_then_ok() {
        let summary = Summary {
            critical: 1,
            warning: 1,
            ok: 21,
            ..Summary::default()
        };
        let counts: Vec<u32> = band_counts(&summary)
            .into_iter()
            .map(|(_, count)| count)
            .collect();
        assert_eq!(counts, [1, 1, 21]);
    }

    #[test]
    fn tiles_count_worst_first_and_ok_last() {
        let summary = Summary {
            critical: 1,
            warning: 2,
            ok: 38,
            ..Summary::default()
        };
        let words: Vec<&str> = tile_parts(&summary, ObjectKind::Services)
            .into_iter()
            .filter(|(_, count, _)| *count > 0)
            .map(|(_, _, word)| word)
            .collect();
        assert_eq!(words, ["critical", "warning", "ok"]);
    }
}
