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
    Metrics, Popover, Segmented, Switch, TextArea, TextField, Tooltip, px,
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
                    .child(if count == 1 {
                        "1 view".to_owned()
                    } else {
                        format!("{count} views")
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
        let title = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.body)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child(model::view_name(view)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(format!("view {} of {count}", index + 1)),
            );
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
            .child(pair(
                Field::new("name").control(TextField::new(&self.name).bordered(true)),
                Field::new("group").control(self.dropdown(
                    "editor-group",
                    &EditorMenu::Group,
                    None,
                    group_name,
                    || self.group_menu(cx),
                    cx,
                )),
            ))
            .child(self.notifications_field(cx))
            .child(self.views_field(cx))
            .child(
                div()
                    .flex_none()
                    .h(px(1.))
                    .mx(px(-PADDING_X))
                    .bg(colors.border_header),
            )
            .child(title)
            .child(Field::new("view name").control(TextField::new(&self.view_name).bordered(true)));
        match view.display {
            ViewDisplay::List | ViewDisplay::GroupedList => self.list_settings(body, view, cx),
            ViewDisplay::HostGroupGrid => self.grid_settings(body, view, cx),
            ViewDisplay::SummaryTiles => self.tiles_settings(body, view, cx),
            ViewDisplay::EventStream => self.stream_settings(body, view, cx),
        }
    }

    // --- The views list --------------------------------------------------

    /// `views` with the list and *add view* (4c).
    fn views_field(&self, cx: &Context<Self>) -> Field {
        let theme = cx.theme();
        let colors = theme.colors;
        let rows: Vec<AnyElement> = self
            .draft
            .views
            .iter()
            .enumerate()
            .map(|(index, view)| self.view_row(index, view, cx))
            .collect();
        let full = self.draft.views.len() >= ic_config::MAX_VIEWS;
        let open = self.menus.is_open(&EditorMenu::AddView);
        let add = div()
            .relative()
            .child(
                div()
                    .id("editor-add-view")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(28.))
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
                slot.child(Popover::new(Self::add_view_menu(cx)).align_right())
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
                            .gap(px(2.))
                            .mx(px(-6.))
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
        let (shows, invalid) = model::shows_text(result, now);
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
            .pl(px(6.))
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
            .min_width(px(220.))
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

    /// *add view* (4d): the display first, each with its icon and what it
    /// shows.
    fn add_view_menu(cx: &Context<Self>) -> Menu {
        let mut menu = Menu::new("add-view-menu")
            .width(px(380.))
            .label("add a view that shows");
        for display in ViewDisplay::ALL {
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("add-view-{display:?}").into()),
                    model::display_name(display),
                )
                .icon(display_icon(display))
                .detail(model::display_detail(display))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.add_view(display, cx);
                })),
            );
        }
        menu.on_dismiss(Self::dismiss_listener(cx))
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
    }

    /// A host-group grid's settings (5e).
    fn grid_settings(&self, body: Stateful<Div>, view: &View, cx: &Context<Self>) -> Stateful<Div> {
        body.child(self.display_field(view, cx))
            .children(self.groups_fields(view, cx))
            .child(self.filter_field(cx))
            .child(
                Field::new("colour by").control(
                    Segmented::new("editor-grid-colour")
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
    }

    /// `display`: a dropdown of the five displays, each with its icon.
    fn display_field(&self, view: &View, cx: &Context<Self>) -> Field {
        Field::new("display").control(self.dropdown(
            "editor-display",
            &EditorMenu::Display,
            Some(display_icon(view.display)),
            model::display_name(view.display).to_owned(),
            || {
                let mut menu = Menu::new("editor-display-menu").min_width(px(240.));
                for display in ViewDisplay::ALL {
                    menu = menu.item(
                        MenuItem::new(
                            ElementId::Name(format!("editor-display-{display:?}").into()),
                            model::display_name(display),
                        )
                        .icon(display_icon(display))
                        .selected(view.display == display)
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
    /// (services only).
    fn grouping_field(view: &View, cx: &Context<Self>) -> Field {
        let hosts = view.object_kind == ObjectKind::Hosts;
        let choices: &[GroupBy] = if hosts {
            &[GroupBy::Host, GroupBy::HostGroup]
        } else {
            &[GroupBy::Host, GroupBy::HostGroup, GroupBy::ServiceGroup]
        };
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
                let group_by = match *index {
                    1 => GroupBy::HostGroup,
                    2 => GroupBy::ServiceGroup,
                    _ => GroupBy::Host,
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
    fn filter_field(&self, cx: &Context<Self>) -> Field {
        let theme = cx.theme();
        let view = self.selected_view();
        let error = self.filter_error();
        let (status, tone) = if let Some(_error) = &error {
            ("invalid".to_owned(), FieldTone::Bad)
        } else if let Some(result) = self.evaluated_view(&view.id) {
            (model::status_text(view, result), FieldTone::Good)
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
                        TextArea::new(&self.filter)
                            .height(filter_height(view.display))
                            .invalid(error.is_some()),
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
        let mut menu = Menu::new("editor-sort-menu").min_width(px(200.));
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
            super::EditorTarget::New => None,
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

    /// A dropdown trigger showing `value` (after `icon`), with `menu` under
    /// it while open.
    fn dropdown(
        &self,
        id: &'static str,
        menu: &EditorMenu,
        icon: Option<IconName>,
        value: String,
        build: impl FnOnce() -> Menu,
        cx: &Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.is_open(menu);
        let toggled = menu.clone();
        div()
            .relative()
            .child(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(theme.metrics.field_height)
                    .px(px(10.))
                    .rounded(theme.metrics.code_radius)
                    .border_1()
                    .border_color(if open {
                        colors.accent
                    } else {
                        colors.border_header
                    })
                    .bg(colors.code_background)
                    .text_size(theme.text.body)
                    .cursor_pointer()
                    .when_some(icon, |trigger, icon| {
                        trigger.child(Icon::new(icon).size(px(13.)).color(colors.text_muted))
                    })
                    .child(div().flex_1().min_w_0().truncate().child(value))
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(12.))
                            .color(colors.text_faint),
                    )
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        this.menus.toggle(toggled.clone(), down_position(event));
                        cx.notify();
                    })),
            )
            .when(open, |slot| slot.child(Popover::new(build())))
            .into_any_element()
    }

    fn group_menu(&self, cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let mut menu = Menu::new("editor-group-menu").min_width(px(240.));
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

    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&ic_ui_kit::Dismissal, &mut Window, &mut App) + 'static {
        cx.listener(|this, dismissal: &ic_ui_kit::Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        })
    }
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
        ViewDisplay::List | ViewDisplay::GroupedList => {
            "host.vars.env == \"prod\" && service.state != 0"
        }
        ViewDisplay::HostGroupGrid => "empty: every host of those groups",
        ViewDisplay::SummaryTiles => "empty: every object of those groups",
        ViewDisplay::EventStream => "empty: every host and service",
    }
}

/// The filter field's height for a view of `display`: three lines for a
/// list's (4c), one for the others, whose filters are short (4e, 5e).
fn filter_height(display: ViewDisplay) -> gpui::Pixels {
    if matches!(display, ViewDisplay::List | ViewDisplay::GroupedList) {
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
