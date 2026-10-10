//! The editor's inspector (4c–4e, 5e): the dashboard's name, group and
//! notifications; its views, each a row with a drag handle, the display's
//! icon, the name, what it shows and `···`, with *add view* under them;
//! and, under a rule, the selected view's settings, which depend on its
//! display.

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Div, ElementId, FontWeight,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    Stateful, StatefulInteractiveElement as _, Styled as _, Window, div,
    prelude::FluentBuilder as _,
};
use ic_config::{
    GridCells, GridColour, GroupBy, GroupSource, HandledMode, HandledSetting, HideHandled,
    ObjectKind, Sort, SortKey, View, ViewDisplay,
};
use ic_model::Timestamp;
use ic_rules::ScopeSetting;
use ic_ui_kit::{
    ActiveTheme as _, Chip, Field, FieldTone, GlyphButton, Icon, IconName, Link, Menu, MenuItem,
    Metrics, Popover, Segmented, Select, Switch, TextArea, TextField, Tooltip, px,
};

use super::{DashboardEditor, EditorEvent, EditorMenu, INSPECTOR_WIDTH, PREVIEW_DEBOUNCE, model};
use crate::dashboard::header::{display_icon, natural_descending};
use crate::menu_state::down_position;

/// Space around the inspector's content (16px above and below, 18px at
/// the sides) and between its fields.
const PADDING_Y: f32 = 16.;
const PADDING_X: f32 = 18.;
const FIELD_GAP: f32 = 16.;

/// A views list row's height (4c).
const VIEW_ROW_HEIGHT: f32 = 34.;

/// The lines an event stream may show before it scrolls (4e).
const STREAM_LINE_CHOICES: [u32; 3] = [8, 15, 30];

/// A view being dragged by its row in the views list: its index, and
/// what the row under the pointer shows.
#[derive(Clone, Debug)]
pub(super) struct DraggedView {
    index: usize,
    name: SharedString,
    display: ViewDisplay,
}

impl Render for DraggedView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(VIEW_ROW_HEIGHT))
            .w(px(INSPECTOR_WIDTH - 2. * PADDING_X))
            .pl(px(6.))
            .pr(px(8.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.accent)
            .bg(colors.row_selected)
            // Drawn outside the window's root, so it sets the font itself.
            .font_family(theme.font_family.clone())
            .text_size(theme.text.body)
            .text_color(colors.text_strong)
            .child(
                Icon::new(IconName::GripVertical)
                    .size(px(13.))
                    .color(colors.text_faint),
            )
            .child(
                Icon::new(display_icon(self.display))
                    .size(px(13.))
                    .color(colors.text_muted),
            )
            .child(self.name.clone())
    }
}

impl DashboardEditor {
    /// The inspector: full height beside the editor's header and preview.
    pub(super) fn render_inspector(&self, window: &Window, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let count = self.draft.views.len();
        let header = div()
            .id("inspector-header")
            .flex()
            .flex_none()
            .items_center()
            .h(Metrics::with_rule(theme.metrics.header_height))
            .px(px(PADDING_X))
            .border_b_1()
            .border_color(colors.border_header)
            .child(
                div()
                    .text_size(theme.text.heading)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child("dashboard"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child({
                        let views = if count == 1 {
                            "1 view".to_owned()
                        } else {
                            format!("{count} views")
                        };
                        if self.edits_health() {
                            format!("built in · {views}")
                        } else {
                            views
                        }
                    }),
            );
        let controls = crate::chrome::Controls::of(window, cx);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(px(INSPECTOR_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(colors.border_split)
            .bg(colors.window_background)
            .child(self.drag.attach(header, controls))
            .child(self.render_inspector_body(cx))
            .child(self.render_inspector_footer(cx))
            .into_any_element()
    }

    /// The inspector's fields, scrolling.
    fn render_inspector_body(&self, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let colors = theme.colors;
        let group_name = self
            .state
            .read(cx)
            .groups()
            .iter()
            .find(|group| group.id == self.draft.group_id)
            .map_or_else(|| "—".to_owned(), |group| group.name.clone());
        let view = self.selected_view();
        let index = self.selected_index();
        let count = self.draft.views.len();
        let health = self.edits_health();
        // A built-in page's name is fixed; it has no group, mark or
        // notifications of its own (16k).
        let name = if health {
            Field::new("name")
                .status("built in", FieldTone::Neutral)
                .control(fixed_field(ic_config::HealthPage::TITLE, cx))
        } else {
            Field::new("name").control(TextField::new(&self.name).bordered(true))
        };
        // The inspector's layout (README §04): the name full width, then
        // the sidebar mark beside the group, the views, and under a rule
        // the selected view's settings, its name first.
        let body = div()
            .id("editor-inspector-body")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(FIELD_GAP))
            .py(px(PADDING_Y))
            .px(px(PADDING_X))
            .overflow_y_scroll()
            .child(name)
            .when(!health, |body| {
                body.child(pair(
                    self.mark_field(cx),
                    Field::new("group").control(self.dropdown(
                        "editor-group",
                        &EditorMenu::Group,
                        None,
                        group_name,
                        || self.group_menu(cx),
                        cx,
                    )),
                ))
            })
            .child(self.views_field(cx))
            // Only problem views notify: a dashboard of handling,
            // downtimes or events views has no notifications row.
            .when(!health && model::notifies(&self.draft.views), |body| {
                body.child(self.notifications_field(cx))
            })
            .child(
                div()
                    .flex_none()
                    .h(px(1.))
                    .mx(px(-PADDING_X))
                    .bg(colors.border_header),
            )
            .child(
                Field::new("view name")
                    .status(format!("view {} of {count}", index + 1), FieldTone::Neutral)
                    .control(TextField::new(&self.view_name).bordered(true)),
            );
        match view.display {
            ViewDisplay::List | ViewDisplay::GroupedList => self.list_settings(body, view, cx),
            ViewDisplay::HostGroupGrid => self.grid_settings(body, view, cx),
            ViewDisplay::SummaryTiles => self.tiles_settings(body, view, cx),
            ViewDisplay::EventStream => self.stream_settings(body, view, cx),
            ViewDisplay::Handling | ViewDisplay::Downtimes => self.threads_settings(body, view, cx),
            ViewDisplay::ZonesAndEndpoints
            | ViewDisplay::Checks
            | ViewDisplay::QueuesAndConnections
            | ViewDisplay::GlobalSwitches => self.health_settings(body, view, cx),
        }
    }

    /// A view of the cluster health page's: its display, and for the tile
    /// views which tiles show and their trend lines (16k).
    fn health_settings(
        &self,
        body: Stateful<Div>,
        view: &View,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let tiles = view.display.health_tiles();
        let body = body.child(self.health_display_field(view, cx));
        if tiles.is_empty() {
            return body;
        }
        let available = self.health_facts(cx).tiles;
        let chips = tiles.iter().map(|&tile| {
            // A tile Icinga has nothing for now (`IcingaDB` while the
            // feature is off) can't be picked: the page wouldn't show it.
            let present = available.contains(&tile);
            let chip = Chip::new(
                ElementId::Name(format!("health-tile-chip:{tile:?}").into()),
                tile.label(),
            )
            .selected(present && view.health.shows(tile))
            .disabled(!present)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                    let hidden = &mut view.health.hidden_tiles;
                    if let Some(index) = hidden.iter().position(|hidden| *hidden == tile) {
                        hidden.remove(index);
                    } else {
                        hidden.push(tile);
                        hidden.sort();
                    }
                });
            }));
            if present {
                chip.into_any_element()
            } else {
                div()
                    .id(ElementId::Name(format!("health-tile-off:{tile:?}").into()))
                    .child(chip)
                    .tooltip(Tooltip::text(format!(
                        "Shows while Icinga reports {} enabled",
                        tile.label()
                    )))
                    .into_any_element()
            }
        });
        body.child(
            Field::new("tiles").control(div().flex().flex_wrap().gap(px(6.)).children(chips)),
        )
        .child(
            Field::new("show").control(
                Switch::new("editor-health-sparklines", view.health.sparklines)
                    .label("sparklines")
                    .on_change(cx.listener(|this, on: &bool, _, cx| {
                        let on = *on;
                        this.change_selected(std::time::Duration::ZERO, cx, |view| {
                            view.health.sparklines = on;
                        });
                    })),
            ),
        )
    }

    /// A health view's `display`: the health kinds, each with its icon; a
    /// kind another view of the page shows can't be picked (one view per
    /// kind).
    fn health_display_field(&self, view: &View, cx: &Context<Self>) -> Field {
        Field::new("display").control(self.dropdown(
            "editor-display",
            &EditorMenu::Display,
            Some(display_icon(view.display)),
            model::display_name(view.display).to_owned(),
            || {
                let mut menu = Menu::new("editor-display-menu").label("health");
                for (display, free) in model::health_displays(&self.draft.views, &view.id) {
                    menu = menu.item(
                        MenuItem::new(
                            ElementId::Name(format!("editor-display-{display:?}").into()),
                            model::display_name(display),
                        )
                        .icon(display_icon(display))
                        .checked(view.display == display)
                        .disabled(!free)
                        .on_click(cx.listener(
                            move |this, _: &ClickEvent, _, cx| {
                                this.menus.close();
                                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                    view.display = display;
                                });
                            },
                        )),
                    );
                }
                menu.on_dismiss(Self::dismiss_listener(cx))
            },
            cx,
        ))
    }

    /// What the health page has on this environment now, for the views
    /// list's counts and the tile chips.
    fn health_facts(&self, cx: &Context<Self>) -> HealthFacts {
        let state = self.state.read(cx);
        let now = Timestamp::now();
        let report = crate::cluster::health::report(
            state.snapshot(),
            crate::cluster::liveness(state.snapshot(), state.connection(), now),
            now,
        );
        HealthFacts {
            endpoints: report.connected + report.not_connected,
            tiles: report
                .checks
                .iter()
                .chain(&report.queues)
                .filter_map(|tile| tile.kind)
                .collect(),
            switches: report.switches.len(),
        }
    }

    /// The health page's pinned parts at the top of its views list: the
    /// alert block and the heartbeat row, locked (trouble stays visible).
    fn pinned_row(
        id: &'static str,
        icon: IconName,
        name: &'static str,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(VIEW_ROW_HEIGHT))
            .pl(px(10.))
            .pr(px(8.))
            .whitespace_nowrap()
            .text_size(theme.text.body)
            .child(
                Icon::new(IconName::Lock)
                    .size(px(13.))
                    .color(colors.text_faint),
            )
            .child(Icon::new(icon).size(px(13.)).color(colors.text_muted))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.text)
                    .child(name),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child("pinned"),
            )
            .tooltip(Tooltip::text(
                "Pinned to the top of the page: trouble stays in sight",
            ))
            .into_any_element()
    }

    /// A row of the health page's views list (16k): the handle, the
    /// kind's icon, the name, what it shows (or `off`) and its switch. A
    /// click selects it; dragging it moves it.
    fn health_view_row(
        &self,
        index: usize,
        view: &View,
        facts: &HealthFacts,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let selected = view.id == self.selected;
        let off = view.health.off;
        let id = view.id.clone();
        let name = model::view_name(view);
        let shows = model::health_shows(view, facts.endpoints, &facts.tiles, facts.switches);
        let dragged = DraggedView {
            index,
            name: name.clone().into(),
            display: view.display,
        };
        let switched = id.clone();
        div()
            .id(ElementId::Name(format!("view-row:{}", view.id).into()))
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(VIEW_ROW_HEIGHT))
            .pl(px(10.))
            .pr(px(8.))
            .rounded(theme.metrics.code_radius)
            .when(selected, |row| row.bg(colors.row_selected))
            .when(!selected, |row| {
                row.hover(|style| style.bg(colors.element_hover))
            })
            .cursor_pointer()
            .whitespace_nowrap()
            .text_size(theme.text.body)
            .child(
                div().flex_none().cursor_grab().child(
                    Icon::new(IconName::GripVertical)
                        .size(px(13.))
                        .color(colors.text_faint),
                ),
            )
            .child(
                Icon::new(display_icon(view.display))
                    .size(px(13.))
                    .color(if off {
                        colors.text_faint
                    } else {
                        colors.text_muted
                    }),
            )
            // The name takes the room (one gap before the count, 16k: no
            // spacer with a gap of its own that would cut it short).
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if off {
                        colors.text_faint
                    } else if selected {
                        colors.text_strong
                    } else {
                        colors.text
                    })
                    .child(name),
            )
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(shows),
            )
            .child(
                div().flex_none().child(
                    Switch::new(
                        ElementId::Name(format!("view-row-switch:{}", view.id).into()),
                        !off,
                    )
                    .on_change(cx.listener(move |this, on: &bool, _, cx| {
                        let on = *on;
                        if let Some(index) = this.index_of(&switched) {
                            this.change_view_at(index, std::time::Duration::ZERO, cx, |view| {
                                view.health.off = !on;
                            });
                        }
                    })),
                ),
            )
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_handle, cx);
                if this.selected != id {
                    this.select(id.clone(), cx);
                }
            }))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedView>(move |style, _, _, _| style.bg(colors.accent_tint_strong))
            .on_drop(cx.listener(move |this, dragged: &DraggedView, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.move_view(dragged.index, index, cx);
            }))
            .into_any_element()
    }

    /// The health page's *add view* (16l): its kinds only; one it has
    /// already is switched on and selected.
    fn add_health_view_menu(cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("add-view-menu").label("health");
        for display in ViewDisplay::HEALTH {
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("add-view-{display:?}").into()),
                    model::display_name(display),
                )
                .icon(display_icon(display))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.add_view(display, cx);
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    // --- The views list --------------------------------------------------

    /// `views` with the list and *add view* (4c).
    fn views_field(&self, cx: &Context<Self>) -> Field {
        let theme = cx.theme();
        let colors = theme.colors;
        let health = self.edits_health();
        let rows: Vec<AnyElement> = if health {
            let facts = self.health_facts(cx);
            [
                Self::pinned_row(
                    "health-pinned-alerts",
                    IconName::TriangleAlert,
                    "alerts",
                    cx,
                ),
                Self::pinned_row(
                    "health-pinned-beats",
                    IconName::HeartPulse,
                    "heartbeats",
                    cx,
                ),
            ]
            .into_iter()
            .chain(
                self.draft
                    .views
                    .iter()
                    .enumerate()
                    .map(|(index, view)| self.health_view_row(index, view, &facts, cx)),
            )
            .collect()
        } else {
            self.draft
                .views
                .iter()
                .enumerate()
                .map(|(index, view)| self.view_row(index, view, cx))
                .collect()
        };
        let full = self.draft.views.len() >= ic_config::MAX_VIEWS;
        let open = self.menus.is_open(&EditorMenu::AddView);
        let add = div()
            .relative()
            .child(
                // A row as wide as the views, its text on their inset;
                // drawn pressed while its menu is open.
                div()
                    .id("editor-add-view")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(28.))
                    .px(px(11.))
                    .rounded(theme.metrics.code_radius)
                    .when(open, |row| row.bg(colors.element_hover))
                    .text_size(theme.text.small)
                    .text_color(if open { colors.text } else { colors.text_muted })
                    .when(!full, |row| {
                        row.cursor_pointer()
                            .hover(|style| style.text_color(colors.text))
                    })
                    .when(full, |row| row.opacity(0.5))
                    .child(Icon::new(IconName::Plus).size(px(13.)))
                    .child("add view")
                    .when(full, |row| {
                        row.tooltip(Tooltip::text(format!(
                            "A dashboard has at most {} views",
                            ic_config::MAX_VIEWS
                        )))
                    })
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .when(!full, |row| {
                        row.on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                            this.menus.toggle(EditorMenu::AddView, down_position(event));
                            cx.notify();
                        }))
                    }),
            )
            .when(open, |slot| {
                slot.child(Popover::new(if health {
                    Self::add_health_view_menu(cx)
                } else {
                    Self::add_view_menu(cx)
                }))
            });
        Field::new("views")
            .status(reorder_hint(), FieldTone::Neutral)
            .control(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("editor-views")
                            .flex()
                            .flex_col()
                            .gap(px(4.))
                            .children(rows),
                    )
                    .child(add),
            )
    }

    /// A row of the views list: the handle, the display's icon, the name,
    /// what the view shows and `···`. A click selects it; dragging it moves
    /// it to the row it is dropped on.
    #[expect(clippy::too_many_lines, reason = "one row, its slots in reading order")]
    fn view_row(&self, index: usize, view: &View, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let selected = view.id == self.selected;
        let id = view.id.clone();
        let now = Timestamp::now();
        let result = self.evaluated_view(&view.id).or_else(|| {
            self.evaluated
                .as_ref()
                .and_then(|(_, result)| result.view(&view.id))
        });
        let (mut shows, invalid) = model::shows_text(result, now);
        if let Some(count) = self.threads_count(view, result, cx) {
            shows = model::threads_text(view.display, count);
        }
        let menu = EditorMenu::ViewOptions(view.id.clone());
        let open = self.menus.is_open(&menu);
        let name = model::view_name(view);
        let dragged = DraggedView {
            index,
            name: name.clone().into(),
            display: view.display,
        };
        let more_id = id.clone();
        let more = GlyphButton::new(
            ElementId::Name(format!("view-row-more:{}", view.id).into()),
            "···",
        )
        .text_size(px(12.))
        .color(colors.text_muted)
        .selected(open)
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            cx.stop_propagation();
            window.focus(&this.focus_handle, cx);
            if this.selected != more_id {
                this.select(more_id.clone(), cx);
            }
            this.menus.toggle(
                EditorMenu::ViewOptions(more_id.clone()),
                down_position(event),
            );
            cx.notify();
        }));
        let row = div()
            .id(ElementId::Name(format!("view-row:{}", view.id).into()))
            .flex()
            .items_center()
            .gap(px(10.))
            .h(px(VIEW_ROW_HEIGHT))
            .pl(px(10.))
            .pr(px(8.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            // A field box, as wide as the fields (README §04).
            .when(selected, |row| {
                row.bg(colors.row_selected)
                    .border_color(colors.row_selected)
            })
            .when(!selected, |row| {
                row.bg(colors.code_background)
                    .border_color(colors.border_header)
                    .hover(|style| style.bg(colors.element_hover))
            })
            .cursor_pointer()
            .whitespace_nowrap()
            .text_size(theme.text.body)
            .child(
                div().flex_none().cursor_grab().child(
                    Icon::new(IconName::GripVertical)
                        .size(px(13.))
                        .color(colors.text_faint),
                ),
            )
            .child(
                Icon::new(display_icon(view.display))
                    .size(px(13.))
                    .color(colors.text_muted),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(if selected {
                        colors.text_strong
                    } else {
                        colors.text
                    })
                    .child(name),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(if invalid {
                        theme.states.text.critical
                    } else {
                        colors.text_faint
                    })
                    .child(shows),
            )
            .child(div().relative().flex_none().child(more).when(open, |slot| {
                slot.child(Popover::new(self.view_menu(index, view, cx)).align_right())
            }))
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                window.focus(&this.focus_handle, cx);
                if this.selected != id {
                    this.select(id.clone(), cx);
                }
            }))
            .on_drag(dragged, |dragged, _, _, cx| cx.new(|_| dragged.clone()))
            .drag_over::<DraggedView>(move |style, _, _, _| style.bg(colors.accent_tint_strong))
            .on_drop(cx.listener(move |this, dragged: &DraggedView, window, cx| {
                window.focus(&this.focus_handle, cx);
                this.move_view(dragged.index, index, cx);
            }));
        row.into_any_element()
    }

    /// A view's `···` (4e): move up and down, duplicate, collapse by
    /// default, remove (no question: *discard* brings it back).
    fn view_menu(&self, index: usize, view: &View, cx: &Context<Self>) -> Menu {
        let count = self.draft.views.len();
        let collapsed = view.collapsed;
        Menu::new("view-row-menu")
            .item(
                MenuItem::new("view-move-up", "move up")
                    .key_hint(move_hint(true))
                    .disabled(index == 0)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Some(to) = index.checked_sub(1) {
                            this.move_view(index, to, cx);
                        }
                    })),
            )
            .item(
                MenuItem::new("view-move-down", "move down")
                    .key_hint(move_hint(false))
                    .disabled(index + 1 >= count)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.move_view(index, index + 1, cx);
                    })),
            )
            .item(
                MenuItem::new("view-duplicate", "duplicate")
                    .disabled(count >= ic_config::MAX_VIEWS)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.duplicate_view(index, cx);
                    })),
            )
            .item(
                MenuItem::new("view-collapse", "collapse by default")
                    .checked(collapsed)
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        this.change_view_at(index, std::time::Duration::ZERO, cx, |view| {
                            view.collapsed = !view.collapsed;
                        });
                    })),
            )
            .separator()
            .item(
                MenuItem::new("view-remove", "remove view")
                    .key_hint(remove_hint())
                    .disabled(count < 2)
                    .when(count < 2, |item| {
                        item.tooltip(Tooltip::new("A dashboard keeps at least one view"))
                    })
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.remove_view(index, cx);
                    })),
            )
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// *add view* (4d, 14-r5-b): every kind, in sections (lists,
    /// overviews, activity), each its icon and name.
    fn add_view_menu(cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("add-view-menu");
        for (position, (section, displays)) in model::DISPLAY_SECTIONS.iter().enumerate() {
            if position > 0 {
                menu = menu.separator();
            }
            menu = menu.label(*section);
            for &display in *displays {
                menu = menu.item(
                    MenuItem::new(
                        ElementId::Name(format!("add-view-{display:?}").into()),
                        model::display_name(display),
                    )
                    .icon(display_icon(display))
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.add_view(display, cx);
                        },
                    )),
                );
            }
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    /// What a handling or downtimes view counts: objects being handled,
    /// downtimes in effect (of its evaluated members); `None` for the
    /// other kinds or before the evaluation.
    fn threads_count(
        &self,
        view: &View,
        result: Option<&ic_core::snapshot::ViewResult>,
        cx: &Context<Self>,
    ) -> Option<usize> {
        let kind = crate::lists::model::ListKind::of_display(view.display)?;
        let members = result?.members()?;
        let state = self.state.read(cx);
        Some(crate::lists::model::count(
            kind,
            state.snapshot(),
            Some(members),
            view.threads.shows,
            Timestamp::now(),
        ))
    }

    // --- A view's settings ---------------------------------------------

    /// A list's or grouped list's settings (4c): display and lists, a
    /// grouped list's grouping, the filter, problems only, handled, sort.
    fn list_settings(&self, body: Stateful<Div>, view: &View, cx: &Context<Self>) -> Stateful<Div> {
        body.child(pair(
            self.display_field(view, cx),
            Self::kind_field(view, cx),
        ))
        .when(view.display == ViewDisplay::GroupedList, |body| {
            body.child(Self::grouping_field(view, cx))
        })
        .child(self.filter_field(cx))
        .child(
            Field::new("show").control(
                Switch::new("editor-problems-only", view.problems_only)
                    .label("problems only")
                    .on_change(cx.listener(|this, on: &bool, _, cx| {
                        let on = *on;
                        this.change_selected(std::time::Duration::ZERO, cx, |view| {
                            view.problems_only = on;
                        });
                    })),
            ),
        )
        .child(self.handled_field(view, cx))
        .child(self.sort_fields(view, cx))
        .child(self.rows_field(view, cx))
    }

    /// A host-group grid's settings (5e).
    fn grid_settings(&self, body: Stateful<Div>, view: &View, cx: &Context<Self>) -> Stateful<Div> {
        body.child(self.display_field(view, cx))
            .children(self.groups_fields(view, cx))
            .child(self.filter_field(cx))
            .child(
                Field::new("colour by").control(
                    Segmented::new("editor-grid-colour")
                        .by_content()
                        .option("worst of host and services")
                        .option("host only")
                        .selected(usize::from(view.grid.colour == GridColour::HostOnly))
                        .on_select(cx.listener(|this, index: &usize, _, cx| {
                            let colour = if *index == 1 {
                                GridColour::HostOnly
                            } else {
                                GridColour::WorstOfHostAndServices
                            };
                            this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                view.grid.colour = colour;
                            });
                        })),
                ),
            )
            .child(
                Field::new("hosts as")
                    .status("squares by default", FieldTone::Neutral)
                    .control(
                        Segmented::new("editor-grid-cells")
                            .option("squares")
                            .option("labelled cells")
                            .selected(usize::from(view.grid.cells == GridCells::LabelledCells))
                            .on_select(cx.listener(|this, index: &usize, _, cx| {
                                let cells = if *index == 1 {
                                    GridCells::LabelledCells
                                } else {
                                    GridCells::Squares
                                };
                                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                    view.grid.cells = cells;
                                });
                            })),
                    ),
            )
            .child(
                Field::new("show").control(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            Switch::new("editor-grid-hide-healthy", view.grid.hide_healthy_groups)
                                .label("hide groups where every host is ok")
                                .on_change(cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                        view.grid.hide_healthy_groups = on;
                                    });
                                })),
                        )
                        .child(
                            Switch::new("editor-grid-each", view.grid.host_in_each_group)
                                .label("a host in several groups shows in each")
                                .on_change(cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                        view.grid.host_in_each_group = on;
                                    });
                                })),
                        ),
                ),
            )
    }

    /// Summary tiles' settings: display and what they count, their groups,
    /// the filter.
    fn tiles_settings(
        &self,
        body: Stateful<Div>,
        view: &View,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        body.child(pair(
            self.display_field(view, cx),
            Self::kind_field(view, cx),
        ))
        .children(self.groups_fields(view, cx))
        .child(self.filter_field(cx))
    }

    /// An event stream's settings (4e): which events, hard states only,
    /// recoveries, and how many lines show before it scrolls.
    fn stream_settings(
        &self,
        body: Stateful<Div>,
        view: &View,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let events = view.stream.events;
        let chip = |id: &'static str, label: &'static str, on: bool, flip: fn(&mut View)| {
            Chip::new(id, label).selected(on).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    this.change_selected(std::time::Duration::ZERO, cx, flip);
                },
            ))
        };
        let lines = STREAM_LINE_CHOICES
            .iter()
            .position(|lines| *lines == view.stream.lines)
            .unwrap_or(usize::MAX);
        body.child(self.display_field(view, cx))
            .child(self.filter_field(cx))
            .child(
                Field::new("events").control(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(px(6.))
                        .child(chip(
                            "stream-state-changes",
                            "state changes",
                            events.state_changes,
                            |view| view.stream.events.state_changes ^= true,
                        ))
                        .child(chip(
                            "stream-acknowledgements",
                            "acknowledgements",
                            events.acknowledgements,
                            |view| view.stream.events.acknowledgements ^= true,
                        ))
                        .child(chip(
                            "stream-downtimes",
                            "downtimes",
                            events.downtimes,
                            |view| view.stream.events.downtimes ^= true,
                        ))
                        .child(chip(
                            "stream-comments",
                            "comments",
                            events.comments,
                            |view| view.stream.events.comments ^= true,
                        ))
                        .child(chip(
                            "stream-flapping",
                            "flapping",
                            events.flapping,
                            |view| view.stream.events.flapping ^= true,
                        )),
                ),
            )
            .child(
                Field::new("show").control(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            Switch::new("editor-stream-hard", view.stream.hard_states_only)
                                .label("hard states only")
                                .on_change(cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                        view.stream.hard_states_only = on;
                                    });
                                })),
                        )
                        .child(
                            Switch::new("editor-stream-recoveries", view.stream.recoveries)
                                .label("include recoveries to OK")
                                .on_change(cx.listener(|this, on: &bool, _, cx| {
                                    let on = *on;
                                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                        view.stream.recoveries = on;
                                    });
                                })),
                        ),
                ),
            )
            .child(
                Field::new("lines").control(
                    STREAM_LINE_CHOICES
                        .iter()
                        .fold(Segmented::new("editor-stream-lines"), |control, lines| {
                            control.option(lines.to_string())
                        })
                        .selected(lines)
                        .on_select(cx.listener(|this, index: &usize, _, cx| {
                            if let Some(lines) = STREAM_LINE_CHOICES.get(*index).copied() {
                                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                    view.stream.lines = lines;
                                });
                            }
                        })),
                ),
            )
            .child(self.rows_field(view, cx))
    }

    /// `display`: a dropdown of the five displays, each with its icon.
    pub(super) fn display_field(&self, view: &View, cx: &Context<Self>) -> Field {
        Field::new("display").control(self.dropdown(
            "editor-display",
            &EditorMenu::Display,
            Some(display_icon(view.display)),
            model::display_name(view.display).to_owned(),
            || {
                let mut menu = Menu::new("editor-display-menu");
                for (position, (section, displays)) in model::DISPLAY_SECTIONS.iter().enumerate() {
                    if position > 0 {
                        menu = menu.separator();
                    }
                    menu = menu.label(*section);
                    for &display in *displays {
                        menu = menu.item(
                            MenuItem::new(
                                ElementId::Name(format!("editor-display-{display:?}").into()),
                                model::display_name(display),
                            )
                            .icon(display_icon(display))
                            .checked(view.display == display)
                            .on_click(cx.listener(
                                move |this, _: &ClickEvent, _, cx| {
                                    this.menus.close();
                                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                        *view = model::with_display(view.clone(), display);
                                    });
                                },
                            )),
                        );
                    }
                }
                menu.on_dismiss(Self::dismiss_listener(cx))
            },
            cx,
        ))
    }

    /// `lists`: services or hosts.
    fn kind_field(view: &View, cx: &Context<Self>) -> Field {
        Field::new("lists").control(
            Segmented::new("editor-kind")
                .option("services")
                .option("hosts")
                .selected(usize::from(view.object_kind == ObjectKind::Hosts))
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    let kind = if *index == 1 {
                        ObjectKind::Hosts
                    } else {
                        ObjectKind::Services
                    };
                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                        *view = model::with_kind(view.clone(), kind);
                    });
                })),
        )
    }

    /// A grouped list's `group by`: host, host group or service group
    /// (services); host group (hosts: by host, each host would be its own
    /// band's only row).
    fn grouping_field(view: &View, cx: &Context<Self>) -> Field {
        let choices = model::groupings(view.object_kind);
        let grouping = view.list_grouping();
        let selected = choices
            .iter()
            .position(|choice| *choice == grouping)
            .unwrap_or(0);
        let control = choices
            .iter()
            .fold(Segmented::new("editor-group-by"), |control, choice| {
                control.option(match choice {
                    GroupBy::HostGroup => "host group",
                    GroupBy::ServiceGroup => "service group",
                    GroupBy::Host | GroupBy::None => "host",
                })
            })
            .selected(selected)
            .on_select(cx.listener(move |this, index: &usize, _, cx| {
                let Some(&group_by) = choices.get(*index) else {
                    return;
                };
                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                    view.group_by = group_by;
                });
            }));
        Field::new("group by").control(control)
    }

    /// A grid's or tiles' groups (5e): by host group (every one, or the
    /// ones picked as chips) or by a custom variable's values.
    fn groups_fields(&self, view: &View, cx: &Context<Self>) -> Vec<Field> {
        let by_var = view.groups.by == GroupSource::CustomVar;
        let source = Field::new("group by").control(
            Segmented::new("editor-group-source")
                .option("host group")
                .option("custom var")
                .selected(usize::from(by_var))
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    let by = if *index == 1 {
                        GroupSource::CustomVar
                    } else {
                        GroupSource::HostGroup
                    };
                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                        view.groups.by = by;
                    });
                })),
        );
        if by_var {
            let empty = view.groups.custom_var_name().is_empty();
            return vec![
                source,
                Field::new("custom var")
                    .status(
                        if empty {
                            "required"
                        } else {
                            "a group per value"
                        },
                        if empty {
                            FieldTone::Bad
                        } else {
                            FieldTone::Neutral
                        },
                    )
                    .control(
                        TextField::new(&self.custom_var)
                            .bordered(true)
                            .invalid(empty),
                    ),
            ];
        }
        vec![source, self.host_groups_field(view, cx)]
    }

    /// `host groups`: a chip per host group, outlined while shown; all of
    /// them (none picked) shows every group, also ones added later.
    fn host_groups_field(&self, view: &View, cx: &Context<Self>) -> Field {
        let state = self.state.read(cx);
        let mut names: Vec<String> = state
            .snapshot()
            .host_groups
            .iter()
            .map(|group| group.name.clone())
            .collect();
        names.sort();
        // Patterns and names Icinga doesn't know (yet) keep a chip, so they
        // can be taken out.
        let extra: Vec<String> = view
            .groups
            .host_groups
            .iter()
            .filter(|name| !names.contains(name))
            .cloned()
            .collect();
        let picked = &view.groups.host_groups;
        let all = picked.is_empty();
        let total = names.len();
        let status = if all {
            format!("all {total} · or pick some")
        } else {
            format!("{} of {total}", picked.len())
        };
        let chips = extra.into_iter().chain(names.iter().cloned()).map(|name| {
            let on = all || picked.contains(&name);
            let names = names.clone();
            let toggled = name.clone();
            Chip::new(
                ElementId::Name(format!("host-group-chip:{name}").into()),
                name,
            )
            .selected(on)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let names = names.clone();
                let toggled = toggled.clone();
                this.change_selected(PREVIEW_DEBOUNCE, cx, move |view| {
                    toggle_group(&mut view.groups.host_groups, &names, &toggled);
                });
            }))
        });
        Field::new("host groups")
            .status(status, FieldTone::Neutral)
            .control(div().flex().flex_wrap().gap(px(6.)).children(chips))
    }

    /// The filter, with the preview's verdict and where an error points.
    pub(super) fn filter_field(&self, cx: &Context<Self>) -> Field {
        let theme = cx.theme();
        let view = self.selected_view();
        let error = self.filter_error();
        let (status, tone) = if let Some(_error) = &error {
            ("invalid".to_owned(), FieldTone::Bad)
        } else if let Some(result) = self.evaluated_view(&view.id) {
            let threads = self.threads_count(view, Some(result), cx);
            // An empty filter is no verdict: what it shows, faint (14-r5-a).
            let tone = if view.filter.trim().is_empty() {
                FieldTone::Neutral
            } else {
                FieldTone::Good
            };
            (model::status_text(view, result, threads), tone)
        } else if self.unavailable {
            ("not checked".to_owned(), FieldTone::Neutral)
        } else {
            ("checking…".to_owned(), FieldTone::Neutral)
        };
        let marker = error.as_deref().and_then(|error| {
            let (line, column) = model::error_position(error)?;
            model::error_marker(&view.filter, line, column, model::MARKER_CHARS)
        });
        Field::new("filter")
            .status(status, tone)
            .control(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        // *copy filter from…* sits in the field's corner.
                        div()
                            .relative()
                            .child(
                                TextArea::new(&self.filter)
                                    .height(filter_height(view.display))
                                    .invalid(error.is_some()),
                            )
                            .child(self.copy_filter_button(cx)),
                    )
                    .when_some(marker, |field, (line, caret)| {
                        field.child(
                            div()
                                .px(px(13.))
                                .text_size(theme.text.small)
                                .text_color(theme.states.text.critical)
                                .child(div().whitespace_nowrap().child(line))
                                .child(div().whitespace_nowrap().child(caret)),
                        )
                    }),
            )
            .error(error)
    }

    /// A list's `handled` (4c): as in settings, show, or hide the kinds
    /// picked; the kinds row is always there, so nothing below it moves
    /// (dim while following the settings, a faint line with *show*).
    fn handled_field(&self, view: &View, cx: &Context<Self>) -> Field {
        let defaults = self.state.read(cx).handled_defaults();
        let mode = view.handled.mode;
        let mode_index = match mode {
            HandledMode::Settings => 0,
            HandledMode::Show => 1,
            HandledMode::Hide => 2,
        };
        let kinds = match mode {
            HandledMode::Settings => defaults,
            HandledMode::Show | HandledMode::Hide => view.handled.hide,
        };
        let second = Self::handled_kinds(mode, kinds, cx);
        Field::new("handled")
            .status(
                if mode == HandledMode::Settings {
                    "follows the settings"
                } else {
                    "set on this view"
                },
                FieldTone::Neutral,
            )
            .control(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        Segmented::new("editor-handled")
                            .option("as in settings")
                            .option("show")
                            .option("hide")
                            .selected(mode_index)
                            .on_select(cx.listener(move |this, index: &usize, _, cx| {
                                let mode = match *index {
                                    1 => HandledMode::Show,
                                    2 => HandledMode::Hide,
                                    _ => HandledMode::Settings,
                                };
                                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                    view.handled = HandledSetting {
                                        mode,
                                        // *hide* starts from what the
                                        // settings hide, the first time.
                                        hide: if mode == HandledMode::Hide
                                            && view.handled.mode != HandledMode::Hide
                                            && view.handled.hide == HideHandled::ALL
                                        {
                                            defaults
                                        } else {
                                            view.handled.hide
                                        },
                                    };
                                });
                            })),
                    )
                    .child(second),
            )
    }

    /// The handled field's second line, always there so nothing below it
    /// moves: the kinds hidden as chips (dim and fixed while following the
    /// settings, picked with *hide*), or a faint line with *show*.
    fn handled_kinds(mode: HandledMode, kinds: HideHandled, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        if mode == HandledMode::Show {
            return div()
                .flex()
                .items_center()
                .h(px(ic_ui_kit::CHIP_HEIGHT))
                .text_size(theme.text.label)
                .text_color(colors.text_faint)
                .child("every handled problem shows, hollow")
                .into_any_element();
        }
        let editable = mode == HandledMode::Hide;
        let chip = |id: &'static str, label: &'static str, on: bool, flip: fn(&mut HideHandled)| {
            Chip::new(id, label)
                .selected(on)
                .disabled(!editable)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                        flip(&mut view.handled.hide);
                    });
                }))
        };
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(ic_ui_kit::CHIP_HEIGHT))
            .child(
                div()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .when(!editable, |label| label.opacity(0.5))
                    .child("hide"),
            )
            .child(chip(
                "handled-acknowledged",
                "acknowledged",
                kinds.acknowledged,
                |kinds| kinds.acknowledged ^= true,
            ))
            .child(chip(
                "handled-downtime",
                "in downtime",
                kinds.in_downtime,
                |kinds| kinds.in_downtime ^= true,
            ))
            .child(chip(
                "handled-host-down",
                "host down",
                kinds.host_down,
                |kinds| kinds.host_down ^= true,
            ))
            .into_any_element()
    }

    /// The sort key and direction, side by side.
    fn sort_fields(&self, view: &View, cx: &Context<Self>) -> Div {
        pair(
            Field::new("sort").control(self.dropdown(
                "editor-sort",
                &EditorMenu::Sort,
                None,
                sort_key_label(view.sort.key, view.object_kind).to_owned(),
                || Self::sort_menu(view, cx),
                cx,
            )),
            Field::new("direction").control(
                Segmented::new("editor-direction")
                    .option("↓ desc")
                    .option("↑ asc")
                    .selected(usize::from(!view.sort.descending))
                    .on_select(cx.listener(|this, index: &usize, _, cx| {
                        let descending = *index == 0;
                        this.change_selected(std::time::Duration::ZERO, cx, |view| {
                            view.sort.descending = descending;
                        });
                    })),
            ),
        )
    }

    fn sort_menu(view: &View, cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("editor-sort-menu");
        for key in [
            SortKey::Severity,
            SortKey::LastStateChange,
            SortKey::Host,
            SortKey::Service,
        ] {
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("editor-sort-{key:?}").into()),
                    sort_key_label(key, view.object_kind),
                )
                .checked(view.sort.key == key)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                        if view.sort.key != key {
                            view.sort = Sort {
                                key,
                                descending: natural_descending(key),
                            };
                        }
                    });
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    /// The dashboard's notifications: inherit, on, off (the group's
    /// setting applies with *inherit*).
    fn notifications_field(&self, cx: &Context<Self>) -> Field {
        let notify_index = match self.draft.notifications {
            ScopeSetting::Inherit | ScopeSetting::Custom(_) => 0,
            ScopeSetting::On => 1,
            ScopeSetting::Off => 2,
        };
        Field::new("notifications").control(
            Segmented::new("editor-notifications")
                .option("inherit")
                .option("on")
                .option("off")
                .selected(notify_index)
                .on_select(cx.listener(|this, index: &usize, _, cx| {
                    this.draft.notifications = match index {
                        1 => ScopeSetting::On,
                        2 => ScopeSetting::Off,
                        _ => ScopeSetting::Inherit,
                    };
                    cx.notify();
                })),
        )
    }

    /// A save error, and *delete dashboard*.
    fn render_inspector_footer(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let colors = theme.colors;
        let reference = match &self.target {
            super::EditorTarget::Existing(reference) => Some(reference.clone()),
            super::EditorTarget::New | super::EditorTarget::Health => None,
        };
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(px(8.))
            .px(px(PADDING_X))
            .py(px(12.))
            .border_t_1()
            .border_color(colors.border_header)
            .when_some(self.save_error.clone(), |footer, error| {
                footer.child(
                    div()
                        .text_size(theme.text.label)
                        .text_color(theme.states.text.critical)
                        .child(error),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(16.))
                    .text_size(theme.text.label)
                    .child(div().flex_1())
                    .when_some(reference, |row, reference| {
                        row.child(
                            Link::new("editor-delete", "delete dashboard")
                                .quiet()
                                .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                    cx.emit(EditorEvent::Delete(reference.clone()));
                                })),
                        )
                    }),
            )
    }

    /// A select showing `value` (after `icon`), its list (`build`) opening
    /// from it while open (the one dropdown system).
    pub(super) fn dropdown(
        &self,
        id: &'static str,
        menu: &EditorMenu,
        icon: Option<IconName>,
        value: String,
        build: impl FnOnce() -> Menu,
        cx: &Context<Self>,
    ) -> AnyElement {
        let open = self.menus.is_open(menu);
        let toggled = menu.clone();
        Select::new(id, value)
            .when_some(icon, Select::icon)
            .open(open)
            .when(open, |select| select.menu(build()))
            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                this.menus.toggle(toggled.clone(), down_position(event));
                cx.notify();
            }))
            .into_any_element()
    }

    fn group_menu(&self, cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let mut menu = Menu::new("editor-group-menu");
        for group in state.groups() {
            let id = group.id.clone();
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("editor-group-{}", group.id).into()),
                    group.name.clone(),
                )
                .checked(group.id == self.draft.group_id)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.draft.group_id.clone_from(&id);
                    cx.notify();
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
    }

    pub(super) fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&ic_ui_kit::Dismissal, &mut Window, &mut App) + 'static {
        cx.listener(|this, dismissal: &ic_ui_kit::Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }
}

/// What the health page has on the environment now: its endpoints, the
/// tiles Icinga has numbers for, its global switches.
struct HealthFacts {
    endpoints: usize,
    tiles: Vec<ic_config::HealthTile>,
    switches: usize,
}

/// A field's box showing `text` that can't be changed (a built-in page's
/// name).
fn fixed_field(text: &'static str, cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .items_center()
        .h(theme.metrics.field_height)
        .px(px(10.))
        .rounded(theme.metrics.code_radius)
        .border_1()
        .border_color(theme.colors.border_header)
        .bg(theme.colors.code_background)
        .text_size(theme.text.body)
        .text_color(theme.colors.text_muted)
        .child(text)
}

/// Two fields side by side, sharing the width (12px apart).
fn pair(left: Field, right: Field) -> Div {
    div()
        .flex()
        .gap(px(12.))
        .child(div().flex_1().min_w_0().child(left))
        .child(div().flex_1().min_w_0().child(right))
}

/// Picks or drops host group `name` of `names` (every group Icinga knows)
/// in a grid's or tiles' list: none picked means every group, so dropping
/// one from all picks the others, and picking the last one back means all
/// again.
fn toggle_group(picked: &mut Vec<String>, names: &[String], name: &str) {
    if picked.is_empty() {
        picked.extend(names.iter().filter(|other| *other != name).cloned());
    } else if let Some(index) = picked.iter().position(|other| other == name) {
        picked.remove(index);
    } else {
        picked.push(name.to_owned());
    }
    if names.iter().all(|other| picked.contains(other))
        && picked.iter().all(|other| names.contains(other))
    {
        picked.clear();
    }
}

/// A sort key's label (`service` reads `name` for host views).
fn sort_key_label(key: SortKey, kind: ObjectKind) -> &'static str {
    match key {
        SortKey::Severity => "severity",
        SortKey::LastStateChange => "last state change",
        SortKey::Host => "host",
        SortKey::Service => match kind {
            ObjectKind::Services => "service",
            ObjectKind::Hosts => "name",
        },
    }
}

/// The filter field's placeholder for a view of `display` (5e: what an
/// empty filter means).
pub(super) fn filter_placeholder(display: ViewDisplay) -> &'static str {
    match display {
        ViewDisplay::List
        | ViewDisplay::GroupedList
        | ViewDisplay::Handling
        | ViewDisplay::Downtimes => "every object",
        ViewDisplay::HostGroupGrid => "empty: every host of those groups",
        ViewDisplay::SummaryTiles => "empty: every object of those groups",
        ViewDisplay::EventStream => "empty: every host and service",
        ViewDisplay::ZonesAndEndpoints
        | ViewDisplay::Checks
        | ViewDisplay::QueuesAndConnections
        | ViewDisplay::GlobalSwitches => "",
    }
}

/// The filter field's height for a view of `display`: three lines for a
/// list's (4c), one for the others, whose filters are short (4e, 5e).
fn filter_height(display: ViewDisplay) -> gpui::Pixels {
    if matches!(
        display,
        ViewDisplay::List
            | ViewDisplay::GroupedList
            | ViewDisplay::Handling
            | ViewDisplay::Downtimes
    ) {
        px(66.)
    } else {
        px(32.)
    }
}

/// The views field's hint: how to reorder.
fn reorder_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "drag or ⌥↑↓ to reorder"
    } else {
        "drag or alt-↑↓ to reorder"
    }
}

/// The key hint of *move up* (`up`) or *move down*.
fn move_hint(up: bool) -> &'static str {
    match (cfg!(target_os = "macos"), up) {
        (true, true) => "⌥↑",
        (true, false) => "⌥↓",
        (false, true) => "alt-↑",
        (false, false) => "alt-↓",
    }
}

/// The key hint of *remove view*.
fn remove_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘⌫"
    } else {
        "ctrl-⌫"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_groups_toggle_between_all_and_picked() {
        let names: Vec<String> = ["a", "b", "c"].map(str::to_owned).to_vec();
        let mut picked = Vec::new();
        toggle_group(&mut picked, &names, "b");
        assert_eq!(picked, ["a", "c"], "dropping one from all picks the rest");
        toggle_group(&mut picked, &names, "a");
        assert_eq!(picked, ["c"]);
        toggle_group(&mut picked, &names, "a");
        toggle_group(&mut picked, &names, "b");
        assert!(picked.is_empty(), "all picked again is all");
        // A pattern keeps its chip and can be taken out.
        let mut picked = vec!["pg-*".to_owned()];
        toggle_group(&mut picked, &names, "pg-*");
        assert!(picked.is_empty());
    }

    #[test]
    fn labels_follow_the_view() {
        assert_eq!(sort_key_label(SortKey::Service, ObjectKind::Hosts), "name");
        assert_eq!(
            sort_key_label(SortKey::Service, ObjectKind::Services),
            "service"
        );
        assert!(reorder_hint().contains("reorder"));
        assert_ne!(move_hint(true), move_hint(false));
        assert!(!remove_hint().is_empty());
    }
}
