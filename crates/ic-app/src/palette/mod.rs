//! The command palette (UI-03), `secondary-k`: turn 1's screen 1d in the
//! v2 look. A field at the top, results grouped by section (commands,
//! dashboards, hosts, services, environments) with state dots, matched
//! characters highlighted and key hints; keyboard only: up/down (or
//! ctrl-p/ctrl-n) move, Enter runs, Tab opens an object as a tab, Escape
//! closes. A query starting with a verb acts on the objects it names
//! (`ack db-prod`).
//!
//! The palette only says what was chosen ([`PaletteEvent::Run`]); the
//! workspace carries it out.

pub(crate) mod fuzzy;
pub(crate) mod model;

use gpui::{
    Action, AnyElement, App, AppContext as _, ClickEvent, Context, Div, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, HighlightStyle, InteractiveElement as _, IntoElement,
    KeyBinding, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Window, div,
    prelude::FluentBuilder as _, px,
};
use ic_model::Timestamp;
use ic_ui_kit::input::{Escape, InputEvent, InputState};
use ic_ui_kit::{ActiveTheme as _, KeyHint, StateDot, TextField, Theme};

pub(crate) use self::model::{Focus, PaletteCommand, PaletteItem, Pause};
use self::model::{PaletteIndex, Section};
use crate::app_state::AppState;
use crate::sidebar::Dot;

/// Key context of the palette.
pub(crate) const PALETTE_CONTEXT: &str = "CommandPalette";

/// Moves the palette's selection down.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct PaletteNext;

/// Moves the palette's selection up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct PalettePrevious;

/// Runs the selected palette item.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct PaletteConfirm;

/// Opens the selected palette item's object as a tab.
#[derive(Clone, Debug, Default, PartialEq, Eq, Action)]
#[action(namespace = icygui)]
pub(crate) struct PaletteOpenTab;

/// Registers the palette's keys. They are bound over the palette's text
/// field (`CommandPalette > Input`), registered after gpui-component's
/// own bindings, so they win over the field's cursor movement.
pub(crate) fn bind_keys(cx: &mut App) {
    let field = Some("CommandPalette > Input");
    let palette = Some(PALETTE_CONTEXT);
    cx.bind_keys([
        KeyBinding::new("down", PaletteNext, field),
        KeyBinding::new("up", PalettePrevious, field),
        KeyBinding::new("ctrl-n", PaletteNext, field),
        KeyBinding::new("ctrl-p", PalettePrevious, field),
        KeyBinding::new("enter", PaletteConfirm, field),
        KeyBinding::new("tab", PaletteOpenTab, field),
        KeyBinding::new("down", PaletteNext, palette),
        KeyBinding::new("up", PalettePrevious, palette),
        KeyBinding::new("enter", PaletteConfirm, palette),
    ]);
}

/// What the palette asks the workspace to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PaletteEvent {
    /// Close it.
    Close,
    /// Run this (and close).
    Run(PaletteCommand),
}

/// The palette view.
pub(crate) struct CommandPalette {
    input: Entity<InputState>,
    index: PaletteIndex,
    query: String,
    items: Vec<PaletteItem>,
    selected: usize,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PaletteEvent> for CommandPalette {}

impl Focusable for CommandPalette {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl CommandPalette {
    /// A palette over what `state` offers now; actions apply to `focus`.
    pub(crate) fn new(
        state: &Entity<AppState>,
        focus: &Focus,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let index = PaletteIndex::build(state.read(cx), focus, Timestamp::now());
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search dashboards, hosts, services, commands…")
        });
        let subscriptions = vec![cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, input, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value().to_string();
                    this.set_query(query);
                    cx.notify();
                }
            },
        )];
        input.update(cx, |input, cx| input.focus(window, cx));
        let items = index.search("");
        Self {
            input,
            index,
            query: String::new(),
            items,
            selected: 0,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// Where the keyboard goes when the palette opens: its field.
    pub(crate) fn default_focus(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }

    /// Searches for `query`.
    pub(crate) fn set_query(&mut self, query: String) {
        self.items = self.index.search(&query);
        self.query = query;
        self.selected = 0;
        self.scroll.scroll_to_item(0);
    }

    /// The results shown.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn items(&self) -> &[PaletteItem] {
        &self.items
    }

    /// The selected result's index.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// The field's state.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    fn move_by(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.items.is_empty() {
            return;
        }
        let count = self.items.len();
        let current = isize::try_from(self.selected).unwrap_or(0);
        let count_signed = isize::try_from(count).unwrap_or(isize::MAX);
        let next = (current + delta).rem_euclid(count_signed);
        self.selected = usize::try_from(next).unwrap_or(0);
        self.scroll.scroll_to_item(self.row_of(self.selected));
        cx.notify();
    }

    /// The scroll child holding item `index` (section headings are
    /// children too).
    fn row_of(&self, index: usize) -> usize {
        let headings = self
            .items
            .iter()
            .take(index + 1)
            .enumerate()
            .filter(|(position, item)| {
                *position == 0 || self.items[position - 1].section != item.section
            })
            .count();
        index + headings
    }

    fn run(&mut self, index: usize, tab: bool, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(index) else {
            return;
        };
        let command = match (&item.command, tab) {
            (PaletteCommand::OpenObject(key), true) => PaletteCommand::OpenTab(key.clone()),
            (command, _) => command.clone(),
        };
        cx.emit(PaletteEvent::Run(command));
    }

    fn on_next(&mut self, _: &PaletteNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(1, cx);
    }

    fn on_previous(&mut self, _: &PalettePrevious, _: &mut Window, cx: &mut Context<Self>) {
        self.move_by(-1, cx);
    }

    fn on_confirm(&mut self, _: &PaletteConfirm, _: &mut Window, cx: &mut Context<Self>) {
        self.run(self.selected, false, cx);
    }

    fn on_open_tab(&mut self, _: &PaletteOpenTab, _: &mut Window, cx: &mut Context<Self>) {
        self.run(self.selected, true, cx);
    }

    fn render_item(&self, index: usize, item: &PaletteItem, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let selected = index == self.selected;
        let label = highlighted(&item.label, &item.matched, theme);
        // An action the API user may not run: faint, with the reason.
        let (label_color, detail) = match &item.denied {
            Some(denial) => (colors.text_faint, denial.clone()),
            None => (colors.text, item.detail.clone()),
        };
        div()
            .id(("palette-item", index))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(34.))
            .px(px(10.))
            .rounded(theme.metrics.code_radius)
            .when(selected, |row| row.bg(colors.row_selected))
            .when(!selected, |row| {
                row.hover(|style| style.bg(colors.row_hover))
            })
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(8.))
                    .justify_center()
                    .when_some(item.dot, |slot, dot| slot.child(dot_of(dot, theme))),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(360.))
                    .truncate()
                    .text_size(theme.text.row)
                    .text_color(label_color)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(detail),
            )
            .when_some(item.key_hint, |row, key| row.child(KeyHint::new(key)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.selected = index;
                this.run(index, false, cx);
            }))
            .into_any_element()
    }
    /// The results, under their section titles.
    fn render_rows(&self, cx: &Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let colors = theme.colors;
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut section: Option<Section> = None;
        for (index, item) in self.items.iter().enumerate() {
            if section != Some(item.section) {
                section = Some(item.section);
                rows.push(
                    div()
                        .flex_none()
                        .px(px(10.))
                        .pt(px(if index == 0 { 4. } else { 10. }))
                        .pb(px(4.))
                        .text_size(theme.text.caption)
                        .text_color(colors.text_faint)
                        .child(item.section.title())
                        .into_any_element(),
                );
            }
            rows.push(self.render_item(index, item, cx));
        }
        rows
    }
}

impl Render for CommandPalette {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.render_rows(cx);
        let theme = cx.theme();
        let colors = theme.colors;
        let count = self.items.len();
        let results = if self.query.trim().is_empty() {
            String::new()
        } else if count == 1 {
            "1 result".to_owned()
        } else {
            format!("{count} results")
        };
        let empty = rows.is_empty();
        div()
            .id("command-palette")
            .key_context(PALETTE_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::on_next))
            .on_action(cx.listener(Self::on_previous))
            .on_action(cx.listener(Self::on_confirm))
            .on_action(cx.listener(Self::on_open_tab))
            .on_action(cx.listener(|_, _: &Escape, _, cx| cx.emit(PaletteEvent::Close)))
            .flex()
            .flex_col()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(10.))
                    .h(px(48.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(colors.border_header)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.accent)
                            .child("›"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(TextField::new(&self.input).text_size(px(14.))),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(theme.text.hint)
                            .text_color(colors.text_faint)
                            .child(results),
                    ),
            )
            .child(
                div()
                    .id("palette-results")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .max_h(px(420.))
                    .p(px(6.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .when(empty, |list| {
                        list.child(
                            div()
                                .py(px(18.))
                                .text_center()
                                .text_size(theme.text.body)
                                .text_color(colors.text_muted)
                                .child("Nothing matches."),
                        )
                    })
                    .children(rows),
            )
            .child(footer(theme))
    }
}

/// The keys the palette takes.
fn footer(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .gap(px(18.))
        .px(px(16.))
        .py(px(10.))
        .border_t_1()
        .border_color(theme.colors.border_header)
        .text_size(theme.text.hint)
        .text_color(theme.colors.text_faint)
        .child("↑↓ navigate")
        .child("↵ run")
        .child("⇥ open as tab")
        .child("ack/dt/check <name> acts")
        .child(div().flex_1())
        .child("esc")
}

/// `label` with the characters at `matched` in the accent colour.
fn highlighted(label: &str, matched: &[usize], theme: &Theme) -> StyledText {
    let mut highlights = Vec::new();
    for (index, (offset, character)) in label.char_indices().enumerate() {
        if matched.contains(&index) {
            highlights.push((
                offset..offset + character.len_utf8(),
                HighlightStyle {
                    color: Some(theme.colors.accent),
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    StyledText::new(SharedString::from(label.to_owned())).with_highlights(highlights)
}

fn dot_of(dot: Dot, theme: &Theme) -> StateDot {
    match dot {
        Dot::State(state) => StateDot::new(state),
        Dot::Ok => StateDot::with_color(theme.states.ok),
        Dot::Empty => StateDot::with_color(theme.states.pending),
    }
    .size(px(7.))
}
