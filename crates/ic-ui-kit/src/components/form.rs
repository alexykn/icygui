//! Form controls styled like the design's inspector (turn 1, screen 1c, in
//! the calmer v2 colours): labelled fields with a status or an error line,
//! switches, segmented choices and a multi-line code field.

use std::fmt;
use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, Entity, Focusable as _, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Pixels, RenderOnce, Role, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, prelude::FluentBuilder as _, px,
    relative,
};
use gpui_component::input::{Textarea, TextareaState};

use crate::theme::{ActiveTheme as _, Theme};

type ToggleHandler = Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>;
type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;
type SelectHandler = Rc<dyn Fn(&usize, &mut Window, &mut App) + 'static>;

/// The tone of a [`Field`]'s status text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FieldTone {
    /// Neutral (faint).
    #[default]
    Neutral,
    /// Good news (`✓ valid · 12 matches`): the OK colour.
    Good,
    /// A problem: the critical colour.
    Bad,
}

impl FieldTone {
    /// The tone's colour.
    #[must_use]
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Neutral => theme.colors.text_faint,
            Self::Good => theme.states.ok,
            Self::Bad => theme.states.critical,
        }
    }
}

/// A labelled form row: the label (with an optional status at its right,
/// like the filter's `✓ valid · 12 matches`), the control, and an error or
/// hint line under it.
#[derive(IntoElement)]
#[must_use = "a field does nothing unless rendered"]
pub struct Field {
    label: SharedString,
    status: Option<(SharedString, FieldTone)>,
    control: Option<AnyElement>,
    error: Option<SharedString>,
    hint: Option<SharedString>,
}

impl Field {
    /// A field labelled `label`.
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            status: None,
            control: None,
            error: None,
            hint: None,
        }
    }

    /// Shows `status` at the right of the label.
    pub fn status(mut self, status: impl Into<SharedString>, tone: FieldTone) -> Self {
        self.status = Some((status.into(), tone));
        self
    }

    /// The control (a text field, a switch, …).
    pub fn control(mut self, control: impl IntoElement) -> Self {
        self.control = Some(control.into_any_element());
        self
    }

    /// Shows `error` under the control, in the critical colour (it replaces
    /// the hint).
    pub fn error(mut self, error: Option<impl Into<SharedString>>) -> Self {
        self.error = error.map(Into::into);
        self
    }

    /// Shows `hint` under the control, faintly.
    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// The label.
    #[must_use]
    pub fn label(&self) -> &SharedString {
        &self.label
    }

    /// The error shown, if any.
    #[must_use]
    pub fn error_text(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }
}

impl fmt::Debug for Field {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Field")
            .field("label", &self.label)
            .field("status", &self.status)
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Field {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let below = match (self.error, self.hint) {
            (Some(error), _) => Some((error, theme.states.critical)),
            (None, Some(hint)) => Some((hint, colors.text_faint)),
            (None, None) => None,
        };
        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(theme.text.label)
                    .child(
                        div()
                            .flex_none()
                            .text_color(colors.text_muted)
                            .child(self.label),
                    )
                    .child(div().flex_1())
                    .when_some(self.status, |row, (status, tone)| {
                        row.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(tone.color(theme))
                                .child(status),
                        )
                    }),
            )
            .children(self.control)
            .when_some(below, |field, (text, color)| {
                field.child(
                    div()
                        .text_size(theme.text.label)
                        .line_height(relative(1.45))
                        .text_color(color)
                        .child(text),
                )
            })
    }
}

/// An on/off switch with a label (`problems only`): a 28×16 track that
/// turns accent-coloured when on.
#[derive(IntoElement)]
#[must_use = "a switch does nothing unless rendered"]
pub struct Switch {
    id: ElementId,
    on: bool,
    label: Option<SharedString>,
    disabled: bool,
    on_change: Option<ToggleHandler>,
}

impl Switch {
    /// A switch that is `on`.
    pub fn new(id: impl Into<ElementId>, on: bool) -> Self {
        Self {
            id: id.into(),
            on,
            label: None,
            disabled: false,
            on_change: None,
        }
    }

    /// Shows `label` beside the switch; clicking it toggles too.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Greys the switch out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` with the new value when clicked (shaped like a GPUI
    /// listener, so `cx.listener(…)` fits).
    pub fn on_change(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Box::new(handler));
        self
    }

    /// Whether it's on.
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.on
    }
}

impl fmt::Debug for Switch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Switch")
            .field("id", &self.id)
            .field("on", &self.on)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Switch {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let on = self.on;
        let enabled = !self.disabled;
        let track = div()
            .relative()
            .flex_none()
            .w(px(28.))
            .h(px(16.))
            .rounded_full()
            .bg(if on {
                colors.accent
            } else {
                colors.element_active
            })
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .when(on, |knob| knob.right(px(2.)))
                    .when(!on, |knob| knob.left(px(2.)))
                    .size(px(12.))
                    .rounded_full()
                    .bg(if on {
                        colors.text_emphasis
                    } else {
                        colors.text_muted
                    }),
            );
        div()
            .id(self.id)
            .role(Role::Switch)
            .flex()
            .items_center()
            .gap(px(8.))
            .text_size(theme.text.small)
            .text_color(if on { colors.text } else { colors.text_muted })
            .child(track)
            .when_some(self.label, gpui::ParentElement::child)
            .when(self.disabled, |row| row.opacity(0.5))
            .when(enabled, gpui::Styled::cursor_pointer)
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.on_change.filter(|_| enabled), |row, handler| {
                row.on_click(move |_: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    handler(&!on, window, cx);
                })
            })
    }
}

/// A row of mutually exclusive choices (`services | hosts`), one selected.
#[derive(IntoElement)]
#[must_use = "a segmented control does nothing unless rendered"]
pub struct Segmented {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    disabled: bool,
    on_select: Option<SelectHandler>,
}

impl Segmented {
    /// A control without options yet.
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            options: Vec::new(),
            selected: 0,
            disabled: false,
            on_select: None,
        }
    }

    /// Adds an option.
    pub fn option(mut self, label: impl Into<SharedString>) -> Self {
        self.options.push(label.into());
        self
    }

    /// Selects the option at `index`.
    pub fn selected(mut self, index: usize) -> Self {
        self.selected = index;
        self
    }

    /// Greys the control out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` with the index of the option clicked (also when it
    /// is selected already; shaped like a GPUI listener).
    pub fn on_select(mut self, handler: impl Fn(&usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// The options' labels.
    #[must_use]
    pub fn options(&self) -> &[SharedString] {
        &self.options
    }
}

impl fmt::Debug for Segmented {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Segmented")
            .field("id", &self.id)
            .field("options", &self.options)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Segmented {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let enabled = !self.disabled;
        let id = self.id.clone();
        let count = self.options.len();
        div()
            .id(self.id)
            .role(Role::RadioGroup)
            .flex()
            .h(theme.metrics.field_height)
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_header)
            .overflow_hidden()
            .text_size(theme.text.small)
            .when(self.disabled, |row| row.opacity(0.5))
            .children(self.options.into_iter().enumerate().map(|(index, label)| {
                let selected = index == self.selected;
                let handler = self.on_select.clone().filter(|_| enabled);
                div()
                    .id(ElementId::NamedInteger(
                        format!("{id}-option").into(),
                        index as u64,
                    ))
                    .role(Role::RadioButton)
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .when(index + 1 < count, |option| {
                        option.border_r_1().border_color(colors.border_header)
                    })
                    .when(selected, |option| {
                        option.bg(colors.element_background).text_color(colors.text)
                    })
                    .when(!selected, |option| {
                        option
                            .text_color(colors.text_muted)
                            .when(enabled, |option| {
                                option.hover(|style| style.text_color(colors.text))
                            })
                    })
                    .when(enabled, gpui::Styled::cursor_pointer)
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                    .when_some(handler, |option, handler| {
                        option.on_click(move |_: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            handler(&index, window, cx);
                        })
                    })
                    .child(label)
            }))
    }
}

/// A [`Chip`]'s height. A row that shows chips in one state and text in
/// another gives the text this height too, so the row keeps its height.
pub const CHIP_HEIGHT: f32 = 22.;

/// A [`Chip`]'s width for `label` at `text_size` (the theme's
/// `text.label`): the monospaced label, the padding and the border. For
/// rows of chips that must fit a width (and never wrap).
#[must_use]
pub fn chip_width(label: &str, text_size: Pixels) -> Pixels {
    #[expect(
        clippy::cast_precision_loss,
        reason = "labels are far shorter than 2^23 characters"
    )]
    let chars = label.chars().count() as f32;
    text_size * (chars * crate::theme::CHAR_WIDTH) + px(2. * CHIP_PADDING + 2.)
}

/// Space left and right of a [`Chip`]'s label.
const CHIP_PADDING: f32 = 8.;

/// A small pill for a quick choice next to a field (`1h`, `2h`, `08:00
/// tomorrow`): it fills the field in, it doesn't hold a state of its own
/// (`selected` shows the choice the field holds).
#[derive(IntoElement)]
#[must_use = "a chip does nothing unless rendered"]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent looks of one chip"
)]
pub struct Chip {
    id: ElementId,
    label: SharedString,
    selected: bool,
    filled: bool,
    marked: bool,
    disabled: bool,
    on_click: Option<ClickHandler>,
}

impl Chip {
    /// A chip labelled `label`.
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            selected: false,
            filled: false,
            marked: false,
            disabled: false,
            on_click: None,
        }
    }

    /// Shows it as the one picked of a row of chips that switch a view
    /// (the notification centre's scopes): the selected-row background of
    /// the lists, filled, its text bright. Its size doesn't change.
    pub fn filled(mut self, filled: bool) -> Self {
        self.filled = filled;
        self
    }

    /// Shows the label in the accent colour (something new behind it):
    /// colour only.
    pub fn marked(mut self, marked: bool) -> Self {
        self.marked = marked;
        self
    }

    /// Shows it as the current choice.
    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Greys it out and ignores clicks.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Runs `handler` when clicked.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    /// The label.
    #[must_use]
    pub fn label(&self) -> &SharedString {
        &self.label
    }
}

impl fmt::Debug for Chip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Chip")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("selected", &self.selected)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let enabled = !self.disabled;
        div()
            .id(self.id)
            .role(Role::Button)
            .aria_label(self.label.clone())
            .flex()
            .flex_none()
            .items_center()
            .h(px(CHIP_HEIGHT))
            .px(px(CHIP_PADDING))
            .rounded(theme.metrics.small_radius)
            .border_1()
            .border_color(if self.selected {
                colors.accent
            } else if self.filled {
                colors.row_selected
            } else {
                colors.border_header
            })
            .bg(if self.filled {
                colors.row_selected
            } else {
                colors.element_background
            })
            .text_size(theme.text.label)
            .text_color(if self.selected || self.marked {
                colors.accent
            } else if self.filled {
                colors.text_strong
            } else {
                colors.text_muted
            })
            .whitespace_nowrap()
            .child(self.label)
            .when(self.disabled, |chip| chip.opacity(0.5))
            .when(enabled, |chip| {
                let marked = self.marked || self.selected;
                let filled = self.filled;
                chip.cursor_pointer()
                    .hover(move |style| {
                        let style = if filled {
                            style
                        } else {
                            style.bg(colors.element_hover)
                        };
                        if marked {
                            style.text_color(colors.accent_hover)
                        } else {
                            style.text_color(colors.text)
                        }
                    })
                    .active(|style| style.bg(colors.element_active))
            })
            // Keep the keyboard in the field it fills.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .when_some(self.on_click.filter(|_| enabled), |chip, handler| {
                chip.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    handler(event, window, cx);
                })
            })
    }
}

/// A multi-line text field on the code surface, for filter expressions:
/// gpui-component's text area in the design's colours. The border turns
/// accent-coloured while it has the focus and critical while `invalid`.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a text area does nothing unless rendered"]
pub struct TextArea {
    state: Entity<TextareaState>,
    height: Pixels,
    invalid: bool,
}

impl TextArea {
    /// A text area for `state`, 96px high.
    pub fn new(state: &Entity<TextareaState>) -> Self {
        Self {
            state: state.clone(),
            height: px(96.),
            invalid: false,
        }
    }

    /// Sets the height of the text (the frame adds its padding).
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// Marks the content as invalid (a critical border).
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }
}

impl RenderOnce for TextArea {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focused = self.state.focus_handle(cx).is_focused(window);
        let theme = cx.theme();
        let colors = theme.colors;
        let border = field_border(theme, focused, self.invalid);
        div()
            .px(px(4.))
            .py(px(4.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(border)
            .bg(colors.code_background)
            .text_color(colors.text_code)
            .child(
                Textarea::new(&self.state)
                    .appearance(false)
                    .h(self.height)
                    .text_size(theme.text.small),
            )
    }
}

/// The border of a framed field: accent while focused, critical while
/// invalid, the header rule otherwise.
pub(crate) fn field_border(theme: &Theme, focused: bool, invalid: bool) -> Hsla {
    if invalid {
        theme.states.critical
    } else if focused {
        theme.colors.accent
    } else {
        theme.colors.border_header
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_show_errors_over_hints() {
        let field = Field::new("url")
            .hint("https://master-01:5665")
            .error(Some("must use https"));
        assert_eq!(field.label(), "url");
        assert_eq!(
            field.error_text().map(AsRef::as_ref),
            Some("must use https")
        );
        let fine = Field::new("name").error(None::<&str>);
        assert!(fine.error_text().is_none());
        assert!(format!("{fine:?}").contains("name"));
    }

    #[test]
    fn tones_use_the_state_colours() {
        let theme = Theme::dark();
        assert_eq!(FieldTone::Good.color(&theme), theme.states.ok);
        assert_eq!(FieldTone::Bad.color(&theme), theme.states.critical);
        assert_eq!(FieldTone::Neutral.color(&theme), theme.colors.text_faint);
    }

    #[test]
    fn segmented_controls_keep_their_options_in_order() {
        let control = Segmented::new("kind")
            .option("services")
            .option("hosts")
            .selected(1);
        assert_eq!(control.options(), ["services", "hosts"]);
        assert_eq!(control.selected, 1);
    }

    #[test]
    fn chips_keep_their_label() {
        let chip = Chip::new("preset", "2h").selected(true);
        assert_eq!(chip.label(), "2h");
        assert!(chip.selected);
        assert!(format!("{chip:?}").contains("2h"));
    }

    #[test]
    fn switches_report_their_state() {
        let switch = Switch::new("problems", true).label("problems only");
        assert!(switch.is_on());
        assert!(format!("{switch:?}").contains("problems only"));
    }

    #[test]
    fn field_borders_prefer_errors_over_focus() {
        let theme = Theme::dark();
        assert_eq!(field_border(&theme, true, true), theme.states.critical);
        assert_eq!(field_border(&theme, true, false), theme.colors.accent);
        assert_eq!(
            field_border(&theme, false, false),
            theme.colors.border_header
        );
    }
}
