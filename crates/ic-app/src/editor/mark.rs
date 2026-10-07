//! The inspector's newer fields (topic 14, rounds 4 and 5): the
//! dashboard's **sidebar mark** (a square previewing it, a dropdown
//! `state | icon`, and with *icon* a searchable picker under the square),
//! **copy filter from…** in the filter field, a view's **rows**, and the
//! settings of handling and downtimes views.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, InteractiveElement as _, IntoElement,
    MouseButton, ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _,
    Styled as _, div, prelude::FluentBuilder as _,
};
use ic_config::{EffectiveMark, RowDensity, SidebarMark, View, ViewDisplay};
use ic_model::{CheckableState, ServiceState};
use ic_ui_kit::{
    ActiveTheme as _, Chip, Field, FieldTone, Icon, IconName, Menu, MenuItem, Popover, Segmented,
    StateDot, TextField, Tooltip, px,
};

use super::model::{self, CopySection};
use super::{DashboardEditor, EditorMenu};
use crate::dashboard::header::display_icon;
use crate::lists::model::{ListKind, Mode, Options};
use crate::menu_state::down_position;

/// The icon picker's grid: columns, and rows before it scrolls.
const PICKER_COLUMNS: usize = 8;
const PICKER_ROWS: usize = 5;
/// A cell of the picker's grid.
const PICKER_CELL: f32 = 34.;
/// *copy filter from…*'s width, and its list's height before it scrolls.
const COPY_WIDTH: f32 = 440.;
const COPY_LIST_HEIGHT: f32 = 340.;

/// What the sidebar mark dropdown says: `state` or `icon`.
fn mark_word(mark: EffectiveMark<'_>) -> &'static str {
    match mark {
        EffectiveMark::State => "state",
        EffectiveMark::Icon(_) | EffectiveMark::KindIcon => "icon",
    }
}

impl DashboardEditor {
    /// The draft's mark as the sidebar will show it.
    fn effective_mark(&self) -> (EffectiveMark<'_>, bool) {
        let problems = self
            .draft
            .views
            .iter()
            .any(|view| view.display.counts_problems());
        let mark = match &self.draft.mark {
            SidebarMark::Icon(name) => EffectiveMark::Icon(name),
            SidebarMark::State | SidebarMark::Auto if problems => EffectiveMark::State,
            SidebarMark::State | SidebarMark::Auto => EffectiveMark::KindIcon,
        };
        (mark, problems)
    }

    /// The icon the mark shows: the one picked, else the first view's
    /// kind's.
    fn mark_icon(&self) -> IconName {
        match self.effective_mark().0 {
            EffectiveMark::Icon(name) => IconName::from_lucide_name(name).unwrap_or(IconName::Box),
            EffectiveMark::State | EffectiveMark::KindIcon => display_icon(
                self.draft
                    .views
                    .first()
                    .map_or(ViewDisplay::List, |view| view.display),
            ),
        }
    }

    /// `sidebar mark` (14-r5-e, f, g): the square previewing the mark (with
    /// *icon*, a click opens the picker under it; with *state* it is no
    /// button), and the dropdown filling the column. *state* is greyed out,
    /// its reason in the tooltip, without a problem view.
    #[expect(
        clippy::too_many_lines,
        reason = "one field, its parts in reading order"
    )]
    pub(super) fn mark_field(&self, cx: &Context<Self>) -> Field {
        let theme = cx.theme();
        let colors = theme.colors;
        let (mark, problems) = self.effective_mark();
        let icon_mark = !matches!(mark, EffectiveMark::State);
        let picker_open = self.menus.is_open(&EditorMenu::IconPicker);
        let size = theme.metrics.field_height;
        let worst = self
            .evaluated
            .as_ref()
            .and_then(|(_, result)| result.summary.worst_unhandled);
        let content: AnyElement = if icon_mark {
            Icon::new(self.mark_icon())
                .size(px(16.))
                .color(colors.text)
                .into_any_element()
        } else {
            match worst {
                Some(state) => StateDot::new(state).size(px(10.)).into_any_element(),
                None => StateDot::new(CheckableState::Service(ServiceState::Ok))
                    .size(px(10.))
                    .into_any_element(),
            }
        };
        let swatch = div()
            .id("editor-mark-swatch")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(size)
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(if picker_open {
                colors.accent
            } else {
                colors.border_header
            })
            .bg(colors.code_background)
            .child(content)
            .when(icon_mark, |swatch| {
                swatch
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.element_hover))
                    .tooltip(Tooltip::text("Pick the icon"))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.menus
                            .toggle(EditorMenu::IconPicker, down_position(event));
                        if this.menus.is_open(&EditorMenu::IconPicker) {
                            this.icon_search.update(cx, |input, cx| {
                                input.set_value("", window, cx);
                                input.focus(window, cx);
                            });
                        }
                        cx.notify();
                    }))
            });
        let swatch = div()
            .relative()
            .flex_none()
            .child(swatch)
            .when(picker_open, |slot| {
                slot.child(Popover::new(self.icon_picker(cx)))
            });
        let dropdown = self.dropdown(
            "editor-mark",
            &EditorMenu::Mark,
            None,
            mark_word(mark).to_owned(),
            || {
                Menu::new("editor-mark-menu")
                    .item(
                        MenuItem::new("editor-mark-state", "state")
                            .checked(!icon_mark)
                            .disabled(!problems)
                            .when(!problems, |item| {
                                item.tooltip(Tooltip::new("no problem view on this dashboard"))
                            })
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.menus.close();
                                this.draft.mark = SidebarMark::State;
                                cx.notify();
                            })),
                    )
                    .item(
                        MenuItem::new("editor-mark-icon", "icon")
                            .checked(icon_mark)
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.menus.close();
                                if !matches!(this.draft.mark, SidebarMark::Icon(_)) {
                                    let icon = this.mark_icon();
                                    this.draft.mark =
                                        SidebarMark::Icon(icon.lucide_name().to_owned());
                                }
                                // Picking *icon* goes on to the icon.
                                this.menus.open(EditorMenu::IconPicker);
                                this.icon_search.update(cx, |input, cx| {
                                    input.set_value("", window, cx);
                                    input.focus(window, cx);
                                });
                                cx.notify();
                            })),
                    )
                    .on_dismiss(Self::dismiss_listener(cx))
            },
            cx,
        );
        Field::new("sidebar mark").control(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .child(swatch)
                .child(div().flex_1().min_w_0().child(dropdown)),
        )
    }

    /// The icon picker (14-r5-f): a search field with the count, the
    /// recent icons, the icons found (eight a row, five rows before it
    /// scrolls) and a footer with the current icon's name and the keys.
    #[expect(
        clippy::too_many_lines,
        reason = "one popover, its parts in reading order"
    )]
    fn icon_picker(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let query = self.icon_search.read(cx).value().to_string();
        let found = model::pickable_icons(&query);
        let total = model::pickable_icons("").len();
        let current = match &self.draft.mark {
            SidebarMark::Icon(name) => IconName::from_lucide_name(name),
            SidebarMark::Auto | SidebarMark::State => None,
        };
        let recent: Vec<IconName> = self
            .state
            .read(cx)
            .recent_icons()
            .iter()
            .filter_map(|name| IconName::from_lucide_name(name))
            .filter(|icon| icon.is_pickable())
            .take(PICKER_COLUMNS)
            .collect();
        let cell = |section: &str, icon: IconName| {
            let selected = current == Some(icon);
            div()
                .id(ElementId::Name(
                    format!("icon-{section}-{}", icon.lucide_name()).into(),
                ))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(PICKER_CELL))
                .rounded(theme.metrics.small_radius)
                .border_1()
                .border_color(if selected {
                    colors.accent
                } else {
                    gpui::transparent_black()
                })
                .when(selected, |cell| cell.bg(colors.row_selected))
                .cursor_pointer()
                .hover(|style| style.bg(colors.element_hover))
                .child(Icon::new(icon).size(px(16.)).color(colors.text))
                .tooltip(Tooltip::text(icon.lucide_name()))
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.pick_icon(icon, cx);
                }))
        };
        let label = |text: &'static str| {
            div()
                .pt(px(6.))
                .pb(px(2.))
                .px(px(3.))
                .text_size(theme.text.label)
                .text_color(colors.text_faint)
                .child(text)
        };
        #[expect(clippy::cast_precision_loss, reason = "a handful of cells")]
        let grid_width = px(PICKER_CELL * PICKER_COLUMNS as f32);
        #[expect(clippy::cast_precision_loss, reason = "a handful of rows")]
        let grid_height = px(PICKER_CELL * PICKER_ROWS as f32);
        let name = current.map_or_else(
            || self.mark_icon().lucide_name().to_owned(),
            |icon| icon.lucide_name().to_owned(),
        );
        div()
            .id("icon-picker")
            .flex()
            .flex_col()
            .w(grid_width + px(2. * 11. + 2.))
            .p(px(11.))
            .gap(px(4.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow_lg()
            .font_family(theme.font_family.clone())
            .text_size(theme.text.small)
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                this.menus.dismiss(event.position);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextField::new(&self.icon_search).bordered(true)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(theme.text.label)
                            .text_color(colors.text_faint)
                            .child(format!("{} of {total}", found.len())),
                    ),
            )
            .when(!recent.is_empty() && query.trim().is_empty(), |picker| {
                picker.child(label("recent")).child(
                    div()
                        .flex()
                        .w(grid_width)
                        .children(recent.iter().map(|icon| cell("recent", *icon))),
                )
            })
            .child(label(if query.trim().is_empty() {
                "icons"
            } else {
                "matching"
            }))
            .child(
                div()
                    .id("icon-grid")
                    .w(grid_width)
                    .h(grid_height)
                    .overflow_y_scroll()
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .w(grid_width)
                            .children(found.iter().map(|icon| cell("all", *icon))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .pt(px(8.))
                    .mt(px(4.))
                    .border_t_1()
                    .border_color(colors.border_header)
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child("↵ choose")
                    .child("esc"),
            )
            .into_any_element()
    }

    /// The filter field's *copy filter from…* button (top right inside the
    /// field), with its popover while open.
    pub(super) fn copy_filter_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.is_open(&EditorMenu::CopyFilter);
        div()
            .absolute()
            .top(px(6.))
            .right(px(6.))
            .child(
                div()
                    .relative()
                    .child(
                        div()
                            .id("editor-copy-filter")
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(22.))
                            .rounded(theme.metrics.small_radius)
                            .when(open, |button| button.bg(colors.element_hover))
                            .cursor_pointer()
                            .hover(|style| style.bg(colors.element_hover))
                            .child(Icon::new(IconName::Copy).size(px(13.)).color(if open {
                                colors.text
                            } else {
                                colors.text_muted
                            }))
                            .tooltip(Tooltip::text("Copy filter from another dashboard or view"))
                            .on_mouse_down(MouseButton::Left, |_, window, _| {
                                window.prevent_default();
                            })
                            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                                this.menus
                                    .toggle(EditorMenu::CopyFilter, down_position(event));
                                if this.menus.is_open(&EditorMenu::CopyFilter) {
                                    this.copy_search.update(cx, |input, cx| {
                                        input.set_value("", window, cx);
                                        input.focus(window, cx);
                                    });
                                }
                                cx.notify();
                            })),
                    )
                    .when(open, |slot| {
                        slot.child(Popover::new(self.copy_filter_list(cx)).align_right())
                    }),
            )
            .into_any_element()
    }

    /// *copy filter from…* (14-r4-e): a search, the other dashboards' and
    /// views' filters (each its name, where it is, and the filter), and
    /// the keys. Choosing one fills the field; the field's undo takes it
    /// back.
    #[expect(
        clippy::too_many_lines,
        reason = "one popover, its parts in reading order"
    )]
    fn copy_filter_list(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let query = self.copy_search.read(cx).value().to_string();
        let sources = self.copy_sources(&query, cx);
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut section = None;
        for (index, source) in sources.into_iter().enumerate() {
            if section != Some(source.section) {
                section = Some(source.section);
                rows.push(
                    div()
                        .px(px(11.))
                        .pt(px(8.))
                        .pb(px(2.))
                        .text_size(theme.text.label)
                        .text_color(colors.text_faint)
                        .child(match source.section {
                            CopySection::Dashboards => "dashboards",
                            CopySection::Views => "views",
                        })
                        .into_any_element(),
                );
            }
            let filter = source.filter.clone();
            rows.push(
                div()
                    .id(ElementId::NamedInteger("copy-source".into(), index as u64))
                    .flex()
                    .gap(px(10.))
                    .px(px(11.))
                    .py(px(6.))
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.element_hover))
                    .child(
                        div().flex_none().pt(px(2.)).child(
                            Icon::new(display_icon(source.display))
                                .size(px(13.))
                                .color(colors.text_muted),
                        ),
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
                                    .gap(px(8.))
                                    .whitespace_nowrap()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_color(colors.text)
                                            .child(source.title),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(colors.text_faint)
                                            .child(source.detail),
                                    ),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(theme.text.label)
                                    .text_color(colors.text_muted)
                                    .child(source.filter),
                            ),
                    )
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.copy_filter(&filter, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty();
        div()
            .id("copy-filter")
            .flex()
            .flex_col()
            .w(px(COPY_WIDTH))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow_lg()
            .font_family(theme.font_family.clone())
            .text_size(theme.text.small)
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                this.menus.dismiss(event.position);
                cx.notify();
            }))
            .child(
                div()
                    .p(px(8.))
                    .border_b_1()
                    .border_color(colors.border_header)
                    .child(TextField::new(&self.copy_search).bordered(true)),
            )
            .child(
                div()
                    .id("copy-filter-list")
                    .max_h(px(COPY_LIST_HEIGHT))
                    .overflow_y_scroll()
                    .pb(px(4.))
                    .children(rows)
                    .when(empty, |list| {
                        list.child(
                            div()
                                .px(px(11.))
                                .py(px(10.))
                                .text_color(colors.text_faint)
                                .child("no other dashboard or view has a filter like that"),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .px(px(11.))
                    .py(px(7.))
                    .border_t_1()
                    .border_color(colors.border_header)
                    .text_size(theme.text.label)
                    .text_color(colors.text_faint)
                    .child("↵ copy into the field")
                    .child(super::undo_hint())
                    .child(div().flex_1())
                    .child("esc"),
            )
            .into_any_element()
    }

    /// A list-like view's `rows` (14-r5-b): as in settings (naming the
    /// settings' density), comfortable, compact.
    pub(super) fn rows_field(&self, view: &View, cx: &Context<Self>) -> Field {
        let global = self.state.read(cx).appearance().row_density;
        let chosen = view.density;
        let item = |id: &'static str, text: String, pick: Option<RowDensity>| {
            MenuItem::new(id, text)
                .checked(chosen == pick)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                        view.density = pick;
                    });
                }))
        };
        Field::new("rows")
            .status(
                if chosen.is_none() {
                    "follows the settings"
                } else {
                    "set on this view"
                },
                FieldTone::Neutral,
            )
            .control(self.dropdown(
                "editor-rows",
                &EditorMenu::Rows,
                None,
                crate::controls::label(chosen, global),
                || {
                    Menu::new("editor-rows-menu")
                        .item(item(
                            "editor-rows-default",
                            crate::controls::label(None, global),
                            None,
                        ))
                        .item(item(
                            "editor-rows-comfortable",
                            "comfortable".to_owned(),
                            Some(RowDensity::Comfortable),
                        ))
                        .item(item(
                            "editor-rows-compact",
                            "compact".to_owned(),
                            Some(RowDensity::Compact),
                        ))
                        .on_dismiss(Self::dismiss_listener(cx))
                },
                cx,
            ))
    }

    /// A handling or downtimes view's settings (14-r4-b, c; 14-r5-g):
    /// display, filter, what it opens with (handling's chip) or as
    /// (downtimes' mode) and which downtimes it shows, its sort, rows,
    /// and that it never counts toward the sidebar number nor notifies.
    pub(super) fn threads_settings(
        &self,
        body: Stateful<Div>,
        view: &View,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let theme = cx.theme();
        let Some(kind) = ListKind::of_display(view.display) else {
            return body;
        };
        let options = Options::of_view(kind, view.threads);
        let body = body
            .child(self.display_field(view, cx))
            .child(self.filter_field(cx));
        let body = match kind {
            ListKind::Handling => body.child(Self::opens_with_field(kind, &options, cx)),
            ListKind::Downtimes => body
                .child(
                    Field::new("opens as").control(
                        Segmented::new("editor-opens-as")
                            .option("timeline")
                            .option("list")
                            .selected(usize::from(options.mode == Mode::List))
                            .on_select(cx.listener(|this, index: &usize, _, cx| {
                                let mode = if *index == 1 {
                                    Mode::List
                                } else {
                                    Mode::Timeline
                                };
                                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                    let mut options =
                                        Options::of_view(ListKind::Downtimes, view.threads);
                                    options.pick_mode(mode);
                                    view.threads = options.to_view();
                                });
                            })),
                    ),
                )
                .child(Self::shows_field(view, cx)),
        };
        let sort = options.sort(kind);
        body.child(
            Field::new("sort")
                .status("the header's sort", FieldTone::Neutral)
                .control(self.dropdown(
                    "editor-thread-sort",
                    &EditorMenu::ThreadSort,
                    None,
                    sort.menu_label(kind).to_owned(),
                    || {
                        let mut menu = Menu::new("editor-thread-sort-menu");
                        for &choice in kind.sorts() {
                            menu = menu.item(
                                MenuItem::new(
                                    SharedString::from(format!("editor-{}", choice.id())),
                                    choice.menu_label(kind),
                                )
                                .checked(choice == sort)
                                .on_click(cx.listener(
                                    move |this, _: &ClickEvent, _, cx| {
                                        this.menus.close();
                                        this.change_selected(
                                            std::time::Duration::ZERO,
                                            cx,
                                            |view| {
                                                let mut options =
                                                    Options::of_view(kind, view.threads);
                                                options.sort = Some(choice);
                                                view.threads = options.to_view();
                                            },
                                        );
                                    },
                                )),
                            );
                        }
                        menu.on_dismiss(Self::dismiss_listener(cx))
                    },
                    cx,
                )),
        )
        .child(self.rows_field(view, cx))
        .child(
            div()
                .text_size(theme.text.label)
                .text_color(theme.colors.text_faint)
                .child(match kind {
                    ListKind::Handling => {
                        "Handling views don't count toward the sidebar number and never \
                         notify; add a problems view for that."
                    }
                    ListKind::Downtimes => {
                        "Downtimes views don't count toward the sidebar number and never \
                         notify; add a problems view for that."
                    }
                }),
        )
    }

    /// Handling's `opens with`: the chip it opens with (the same setting
    /// as the header's chips).
    fn opens_with_field(kind: ListKind, options: &Options, cx: &Context<Self>) -> Field {
        let chips = kind.chips().iter().map(|&chip| {
            Chip::new(
                SharedString::from(format!("editor-opens-with-{}", chip.id())),
                chip.label(kind),
            )
            .selected(options.chip == chip)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                    let mut options = Options::of_view(kind, view.threads);
                    options.pick_chip(chip);
                    view.threads = options.to_view();
                });
            }))
        });
        Field::new("opens with").control(div().flex().flex_wrap().gap(px(6.)).children(chips))
    }

    /// Downtimes' `shows`: in effect, upcoming, from config (at least one).
    fn shows_field(view: &View, cx: &Context<Self>) -> Field {
        let shows = view.threads.shows;
        let chip = |id: &'static str,
                    label: &'static str,
                    on: bool,
                    flip: fn(&mut ic_config::DowntimeKinds)| {
            Chip::new(id, label).selected(on).on_click(cx.listener(
                move |this, _: &ClickEvent, _, cx| {
                    this.change_selected(std::time::Duration::ZERO, cx, |view| {
                        let mut shows = view.threads.shows;
                        flip(&mut shows);
                        // Showing nothing would be a view of nothing.
                        if shows.in_effect || shows.upcoming || shows.from_config {
                            view.threads.shows = shows;
                        }
                    });
                },
            ))
        };
        Field::new("shows").control(
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .child(chip(
                    "editor-shows-in-effect",
                    "in effect",
                    shows.in_effect,
                    |shows| shows.in_effect ^= true,
                ))
                .child(chip(
                    "editor-shows-upcoming",
                    "upcoming",
                    shows.upcoming,
                    |shows| shows.upcoming ^= true,
                ))
                .child(chip(
                    "editor-shows-from-config",
                    "from config",
                    shows.from_config,
                    |shows| shows.from_config ^= true,
                )),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dropdown_names_the_mark() {
        assert_eq!(mark_word(EffectiveMark::State), "state");
        assert_eq!(mark_word(EffectiveMark::Icon("server")), "icon");
        assert_eq!(mark_word(EffectiveMark::KindIcon), "icon");
    }
}
