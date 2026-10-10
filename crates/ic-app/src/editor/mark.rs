//! The inspector's newer fields (topic 14, rounds 4 and 5): the
//! dashboard's **sidebar mark** (a square previewing it, a dropdown
//! `state | icon`, and with *icon* a searchable picker under the square),
//! **copy filter from…** in the filter field, a view's **rows**, and the
//! settings of handling and downtimes views.

use gpui::{
    AnyElement, ClickEvent, Context, Div, ElementId, InteractiveElement as _, IntoElement,
    Keystroke, MouseButton, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, div, prelude::FluentBuilder as _,
};
use ic_config::{EffectiveMark, RowDensity, SidebarMark, View, ViewDisplay};
use ic_model::{CheckableState, ServiceState};
use ic_ui_kit::{
    ActiveTheme as _, Chip, Dismissable, Field, FieldTone, Icon, IconName, Menu, MenuItem, Popover,
    Segmented, StateDot, TextField, Tooltip, TooltipPlacement, px,
};

use super::model::{self, CopySection};
use super::{DashboardEditor, EditorMenu};
use crate::dashboard::header::display_icon;
use crate::lists::model::{ListKind, Mode, Options};
use crate::menu_state::down_position;

/// The icon picker's grid: columns, and rows before it scrolls.
pub(super) const PICKER_COLUMNS: usize = 8;
const PICKER_ROWS: usize = 5;
/// A cell of the picker's grid.
const PICKER_CELL: f32 = 34.;
/// *copy filter from…*'s width, and its list's height before it scrolls.
const COPY_WIDTH: f32 = 440.;
const COPY_LIST_HEIGHT: f32 = 340.;
/// The width of *copy filter from…*'s preview card.
const COPY_PREVIEW_WIDTH: f32 = 260.;

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

    /// The mark the sidebar row will show: the worst state's dot of the
    /// latest evaluation, or the icon.
    pub(super) fn sidebar_mark(&self) -> crate::sidebar::model::Mark {
        use crate::sidebar::model::{Dot, Mark};
        match self.effective_mark().0 {
            EffectiveMark::State => Mark::Dot(Dot::from_summary(
                self.evaluated.as_ref().map(|(_, result)| &result.summary),
            )),
            EffectiveMark::Icon(_) | EffectiveMark::KindIcon => Mark::Icon(self.mark_icon()),
        }
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
    /// button), and the select filling the column. *state* is greyed out
    /// without a problem view, its reason in a tooltip left of the list
    /// (so it never covers the *icon* row).
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
            .bg(if picker_open {
                colors.element_hover
            } else {
                colors.code_background
            })
            .child(content)
            .when(icon_mark, |swatch| {
                swatch
                    .cursor_pointer()
                    .hover(|style| style.bg(colors.element_hover))
                    .when(!picker_open, |swatch| {
                        swatch.tooltip(Tooltip::text("Pick the icon"))
                    })
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                        this.menus
                            .toggle(EditorMenu::IconPicker, down_position(event));
                        if this.menus.is_open(&EditorMenu::IconPicker) {
                            this.start_icon_picker(window, cx);
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
                                item.tooltip(
                                    Tooltip::new("no problem view on this dashboard")
                                        .placement(TooltipPlacement::Left),
                                )
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
                                this.start_icon_picker(window, cx);
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

    /// Opens the icon picker: an empty search with the keyboard in it,
    /// the cursor on the current icon.
    fn start_icon_picker(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.icon_search.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        let current = self.mark_icon();
        self.icon_cursor = model::pickable_icons("")
            .iter()
            .position(|icon| *icon == current)
            .or(Some(0));
        if let Some(cursor) = self.icon_cursor {
            self.icon_scroll.scroll_to_item(cursor / PICKER_COLUMNS);
        }
    }

    /// A key while the icon picker is open: the arrows move the cursor
    /// over the icons found (a row is eight), Enter chooses the one under
    /// it. Returns whether the key was used.
    fn icon_picker_key(&mut self, keystroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let query = self.icon_search.read(cx).value().to_string();
        let found = model::pickable_icons(&query);
        if found.is_empty() {
            return false;
        }
        let last = found.len() - 1;
        let at = self.icon_cursor.unwrap_or(0).min(last);
        let moved = match keystroke.key.as_str() {
            "left" => at.saturating_sub(1),
            "right" => (at + 1).min(last),
            "up" => at.checked_sub(PICKER_COLUMNS).unwrap_or(at),
            "down" => {
                if at + PICKER_COLUMNS <= last {
                    at + PICKER_COLUMNS
                } else {
                    at
                }
            }
            "enter" => {
                self.pick_icon(found[at], cx);
                return true;
            }
            _ => return false,
        };
        self.icon_cursor = Some(moved);
        self.icon_scroll.scroll_to_item(moved / PICKER_COLUMNS);
        cx.notify();
        true
    }

    /// The icon picker (14-r5-f): a search field with the magnifier and
    /// the count inside it, the recent icons, the icons found (eight a row,
    /// five rows before it scrolls) and a footer naming the icon under the
    /// cursor, with the keys.
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
        let cursor = self
            .icon_cursor
            .filter(|cursor| *cursor < found.len())
            .or_else(|| (!found.is_empty()).then_some(0));
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
        let cell = |section: &str, icon: IconName, under_cursor: bool| {
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
                .when(under_cursor, |cell| cell.bg(colors.element_hover))
                .cursor_pointer()
                .hover(|style| style.bg(colors.element_hover))
                .child(Icon::new(icon).size(px(16.)).color(colors.text))
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
        // The footer names the icon under the cursor (else the current).
        let name = cursor.and_then(|cursor| found.get(cursor)).map_or_else(
            || self.mark_icon().lucide_name().to_owned(),
            |icon| icon.lucide_name().to_owned(),
        );
        let rows: Vec<AnyElement> = found
            .chunks(PICKER_COLUMNS)
            .enumerate()
            .map(|(row, icons)| {
                div()
                    .flex()
                    .flex_none()
                    .w(grid_width)
                    .children(icons.iter().enumerate().map(|(column, icon)| {
                        cell("all", *icon, cursor == Some(row * PICKER_COLUMNS + column))
                    }))
                    .into_any_element()
            })
            .collect();
        let card = div()
            .id("icon-picker")
            .occlude()
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
            .child(
                TextField::new(&self.icon_search)
                    .bordered(true)
                    .leading(
                        Icon::new(IconName::Search)
                            .size(px(14.))
                            .color(colors.text_muted),
                    )
                    .trailing(format!("{} of {total}", found.len())),
            )
            .when(!recent.is_empty(), |picker| {
                picker.child(label("recent")).child(
                    div()
                        .flex()
                        .w(grid_width)
                        .children(recent.iter().map(|icon| cell("recent", *icon, false))),
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
                    .flex()
                    .flex_col()
                    .w(grid_width)
                    .h(grid_height)
                    .overflow_y_scroll()
                    .track_scroll(&self.icon_scroll)
                    .children(rows),
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
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(colors.text)
                            .child(name),
                    )
                    .child("↑↓←→ move")
                    .child("↵ choose")
                    .child("esc"),
            );
        let editor = cx.entity().downgrade();
        Dismissable::new("icon-picker-popup", card, Self::dismiss_listener(cx))
            .on_key(move |keystroke, _, cx| {
                editor
                    .update(cx, |this, cx| this.icon_picker_key(keystroke, cx))
                    .unwrap_or(false)
            })
            .into_any_element()
    }

    /// The filter field's *copy filter from…* button (top right inside the
    /// field, drawn pressed while open), with its popover.
    pub(super) fn copy_filter_button(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let open = self.menus.is_open(&EditorMenu::CopyFilter);
        let button = div()
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
            .when(!open, |button| {
                button.tooltip(Tooltip::text("Copy filter from another dashboard or view"))
            })
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
                    this.set_copy_cursor(None, cx);
                }
                cx.notify();
            }));
        div()
            .absolute()
            .top(px(6.))
            .right(px(6.))
            .child(div().relative().child(button).when(open, |slot| {
                slot.child(Popover::new(self.copy_filter_list(cx)).align_right())
            }))
            .into_any_element()
    }

    /// Moves *copy filter from…*'s cursor to `index` (of the filters found
    /// for the search) and counts what that filter matches here, for the
    /// preview card (once per filter: on this environment's snapshot, no
    /// request).
    pub(super) fn set_copy_cursor(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.copy_cursor = index;
        let Some(index) = index else {
            return;
        };
        let query = self.copy_search.read(cx).value().to_string();
        let Some(source) = self.copy_sources(&query, cx).into_iter().nth(index) else {
            return;
        };
        if self
            .copy_preview
            .as_ref()
            .is_some_and(|(filter, _)| *filter == source.filter)
        {
            return;
        }
        let snapshot = self.state.read(cx).snapshot().clone();
        let count = model::count_matches(
            &snapshot,
            &source.filter,
            source.display,
            source.object_kind,
            ic_model::Timestamp::now(),
        );
        self.copy_preview = count.map(|count| (source.filter, count));
    }

    /// A key while *copy filter from…* is open: ↑ ↓ move the cursor over
    /// the filters found, Enter copies the one under it (else the first).
    /// Returns whether the key was used.
    fn copy_filter_key(
        &mut self,
        keystroke: &Keystroke,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let query = self.copy_search.read(cx).value().to_string();
        let sources = self.copy_sources(&query, cx);
        if sources.is_empty() {
            return false;
        }
        let last = sources.len() - 1;
        let moved = match (keystroke.key.as_str(), self.copy_cursor) {
            ("down", None) => 0,
            ("down", Some(at)) => (at + 1).min(last),
            ("up", None) => last,
            ("up", Some(at)) => at.saturating_sub(1),
            ("enter", at) => {
                let filter = sources[at.unwrap_or(0).min(last)].filter.clone();
                self.copy_filter(&filter, window, cx);
                return true;
            }
            _ => return false,
        };
        self.set_copy_cursor(Some(moved), cx);
        // Two section labels at most come before a row.
        self.copy_scroll.scroll_to_item(moved + 2);
        cx.notify();
        true
    }

    /// *copy filter from…* (14-r4-e): a search with its magnifier, the
    /// other dashboards' and views' filters (each its sidebar mark or its
    /// kind's icon, its name, where it is, and the filter), and the keys.
    /// The row under the cursor shows a preview card left of the list:
    /// what it is, its filter, and how many objects it matches here.
    /// Choosing one fills the field; the field's undo takes it back.
    #[expect(
        clippy::too_many_lines,
        reason = "one popover, its parts in reading order"
    )]
    fn copy_filter_list(&self, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let query = self.copy_search.read(cx).value().to_string();
        let sources = self.copy_sources(&query, cx);
        let cursor = self.copy_cursor.filter(|cursor| *cursor < sources.len());
        // A dashboard's row shows its sidebar mark; a view's its kind.
        let state = self.state.read(cx);
        let now = ic_model::Timestamp::now();
        let sidebar_mark = |reference: &ic_rules::DashboardRef| {
            let dashboard = state
                .groups()
                .iter()
                .find(|group| group.id == reference.group_id)?
                .dashboards
                .iter()
                .find(|dashboard| dashboard.id == reference.dashboard_id)?;
            Some(
                crate::sidebar::model::mark_and_count(
                    dashboard,
                    state.result(reference),
                    state.snapshot(),
                    now,
                )
                .0,
            )
        };
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
            let under_cursor = cursor == Some(index);
            let preview = under_cursor.then(|| self.copy_preview_card(&source, cx));
            let filter = source.filter.clone();
            let lead = match source.dashboard.as_ref().and_then(&sidebar_mark) {
                Some(mark) => crate::sidebar::mark(mark, theme),
                None => Icon::new(display_icon(source.display))
                    .size(px(13.))
                    .color(colors.text_muted)
                    .into_any_element(),
            };
            rows.push(
                div()
                    .id(ElementId::NamedInteger("copy-source".into(), index as u64))
                    .relative()
                    .flex()
                    .gap(px(10.))
                    .px(px(11.))
                    .py(px(6.))
                    .cursor_pointer()
                    .when(under_cursor, |row| row.bg(colors.element_hover))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .justify_center()
                            .w(px(13.))
                            .pt(px(2.))
                            .child(lead),
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
                    .children(preview)
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.copy_cursor != Some(index) {
                            this.set_copy_cursor(Some(index), cx);
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.copy_filter(&filter, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty();
        let card = div()
            .id("copy-filter")
            .occlude()
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(36.))
                    .px(px(11.))
                    .border_b_1()
                    .border_color(colors.border_header)
                    .child(
                        TextField::new(&self.copy_search)
                            .text_size(theme.text.body)
                            .leading(
                                Icon::new(IconName::Search)
                                    .size(px(13.))
                                    .color(colors.text_muted),
                            ),
                    ),
            )
            .child(
                div()
                    .id("copy-filter-list")
                    .flex()
                    .flex_col()
                    .max_h(px(COPY_LIST_HEIGHT))
                    .overflow_y_scroll()
                    .track_scroll(&self.copy_scroll)
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
            );
        let editor = cx.entity().downgrade();
        Dismissable::new("copy-filter-popup", card, Self::dismiss_listener(cx))
            .on_key(move |keystroke, window, cx| {
                editor
                    .update(cx, |this, cx| this.copy_filter_key(keystroke, window, cx))
                    .unwrap_or(false)
            })
            .into_any_element()
    }

    /// The preview card beside the row under *copy filter from…*'s cursor
    /// (14-r4-e): its name, what it is, its filter, and how many objects
    /// that matches here.
    fn copy_preview_card(&self, source: &model::CopySource, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let count = self
            .copy_preview
            .as_ref()
            .filter(|(filter, _)| *filter == source.filter)
            .map(|(_, count)| *count);
        let card = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(8.))
            .w(px(COPY_PREVIEW_WIDTH))
            .p(px(11.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow_lg()
            .font_family(theme.font_family.clone())
            .text_size(theme.text.small)
            .child(
                div()
                    .truncate()
                    .text_size(theme.text.body)
                    .text_color(colors.text_strong)
                    .child(source.title.clone()),
            )
            .child(
                div()
                    .text_color(colors.text_faint)
                    .child(model::copy_source_kind(source)),
            )
            .child(
                div()
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(theme.metrics.small_radius)
                    .bg(colors.code_background)
                    .text_color(colors.text_code)
                    .child(source.filter.clone()),
            )
            .child(div().text_color(colors.text).child(match count {
                Some(1) => "here: matches 1 object".to_owned(),
                Some(count) => format!("here: matches {count} objects"),
                None => "here: the filter doesn't parse".to_owned(),
            }));
        Popover::new(card).left().gap(px(12.)).into_any_element()
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
        body.child(Field::new("sort").control(self.dropdown(
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
                                this.change_selected(std::time::Duration::ZERO, cx, |view| {
                                    let mut options = Options::of_view(kind, view.threads);
                                    options.sort = Some(choice);
                                    view.threads = options.to_view();
                                });
                            },
                        )),
                    );
                }
                menu.on_dismiss(Self::dismiss_listener(cx))
            },
            cx,
        )))
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
