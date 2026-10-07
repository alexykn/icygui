//! State indicators: the list's state circles and the sidebar's dots.

use gpui::{
    App, FontWeight, Hsla, IntoElement, ParentElement as _, Pixels, RenderOnce, SharedString,
    Styled as _, Window, div, prelude::FluentBuilder as _, px,
};
use ic_model::CheckableState;

use crate::theme::{ActiveTheme as _, Metrics, Theme};

/// What colour an indicator takes: a host or service state, or any colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    /// The state's colour from [`crate::StateColors`].
    State(CheckableState),
    /// A fixed colour.
    Color(Hsla),
}

impl Paint {
    /// The colour in `theme`.
    #[must_use]
    pub fn resolve(self, theme: &Theme) -> Hsla {
        match self {
            Self::State(state) => theme.states.checkable(state),
            Self::Color(color) => color,
        }
    }
}

impl From<CheckableState> for Paint {
    fn from(state: CheckableState) -> Self {
        Self::State(state)
    }
}

impl From<Hsla> for Paint {
    fn from(color: Hsla) -> Self {
        Self::Color(color)
    }
}

/// Where a [`StateCircle`] is used; picks its diameter and ring width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CircleSize {
    /// List rows (22px).
    #[default]
    Row,
    /// The host pane's service rows (14px).
    Compact,
    /// The detail pane header (34px).
    Pane,
}

impl CircleSize {
    /// Diameter and ring width (for handled problems) in `metrics`.
    #[must_use]
    pub fn dimensions(self, metrics: &Metrics) -> (Pixels, Pixels) {
        match self {
            Self::Row => (metrics.row_circle, metrics.row_ring),
            Self::Compact => (metrics.compact_circle, metrics.compact_ring),
            Self::Pane => (metrics.pane_circle, metrics.pane_ring),
        }
    }
}

#[derive(Clone, Debug)]
enum Caption {
    /// Muted text under the circle (time in state).
    Muted(SharedString),
    /// The state's short label in its colour (`CRIT`).
    StateLabel,
}

/// Icinga Web's state circle: filled for unhandled problems, a hollow ring
/// for handled ones (acknowledged or in downtime), optionally with a caption
/// underneath.
///
/// ```text
/// StateCircle::new(CheckableState::Service(ServiceState::Critical))
///     .handled(service.is_handled(host_down))
///     .caption("14m")
/// ```
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a state circle does nothing unless rendered"]
pub struct StateCircle {
    paint: Paint,
    state: Option<CheckableState>,
    size: CircleSize,
    handled: bool,
    caption: Option<Caption>,
}

impl StateCircle {
    /// A filled circle in the state's colour.
    pub fn new(state: CheckableState) -> Self {
        Self {
            paint: Paint::State(state),
            state: Some(state),
            size: CircleSize::default(),
            handled: false,
            caption: None,
        }
    }

    /// A filled circle in any colour.
    pub fn with_color(color: Hsla) -> Self {
        Self {
            paint: Paint::Color(color),
            state: None,
            size: CircleSize::default(),
            handled: false,
            caption: None,
        }
    }

    /// Sets the size preset.
    pub fn size(mut self, size: CircleSize) -> Self {
        self.size = size;
        self
    }

    /// Draws a hollow ring instead of a filled circle: the problem is
    /// acknowledged or in downtime.
    pub fn handled(mut self, handled: bool) -> Self {
        self.handled = handled;
        self
    }

    /// Shows muted text under the circle, such as the time in state.
    pub fn caption(mut self, text: impl Into<SharedString>) -> Self {
        self.caption = Some(Caption::Muted(text.into()));
        self
    }

    /// Shows the state's short label (`CRIT`, `UP`) under the circle, in the
    /// state's colour. Does nothing for circles built with
    /// [`StateCircle::with_color`].
    pub fn state_label(mut self) -> Self {
        self.caption = Some(Caption::StateLabel);
        self
    }
}

impl RenderOnce for StateCircle {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = self.paint.resolve(theme);
        let (diameter, ring) = self.size.dimensions(&theme.metrics);
        let circle = div()
            .flex_none()
            .size(diameter)
            .rounded_full()
            .map(|circle| {
                if self.handled {
                    circle.border(ring).border_color(color)
                } else {
                    circle.bg(color)
                }
            });
        let caption = match self.caption {
            None => None,
            Some(Caption::Muted(text)) => Some(
                div()
                    .text_size(theme.text.caption)
                    .text_color(theme.colors.text_muted)
                    .whitespace_nowrap()
                    .child(text),
            ),
            Some(Caption::StateLabel) => self.state.map(|state| {
                div()
                    .text_size(theme.text.caption)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(color)
                    .whitespace_nowrap()
                    .child(state.short_label())
            }),
        };
        match caption {
            None => circle.into_any_element(),
            Some(caption) => div()
                .flex()
                .flex_col()
                .flex_none()
                .items_center()
                .gap(px(4.))
                .child(circle)
                .child(caption)
                .into_any_element(),
        }
    }
}

/// A small filled dot: a dashboard's worst state in the sidebar, the summary
/// bar's legend, the connection status.
#[derive(Clone, Debug, IntoElement)]
#[must_use = "a state dot does nothing unless rendered"]
pub struct StateDot {
    paint: Paint,
    size: Option<Pixels>,
    hollow: bool,
}

impl StateDot {
    /// A dot in the state's colour.
    pub fn new(state: CheckableState) -> Self {
        Self {
            paint: Paint::State(state),
            size: None,
            hollow: false,
        }
    }

    /// A dot in any colour.
    pub fn with_color(color: Hsla) -> Self {
        Self {
            paint: Paint::Color(color),
            size: None,
            hollow: false,
        }
    }

    /// Sets the diameter (default: [`Metrics::sidebar_dot`]).
    pub fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }

    /// Draws a ring instead of a filled dot, as [`StateCircle::handled`]
    /// does for handled problems: something that is there but quieter (a
    /// notification recorded without a system notification). Same size.
    pub fn hollow(mut self, hollow: bool) -> Self {
        self.hollow = hollow;
        self
    }
}

impl RenderOnce for StateDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let color = self.paint.resolve(theme);
        let dot = div()
            .flex_none()
            .size(self.size.unwrap_or(theme.metrics.sidebar_dot))
            .rounded_full();
        if self.hollow {
            dot.border(px(1.5)).border_color(color)
        } else {
            dot.bg(color)
        }
    }
}

#[cfg(test)]
mod tests {
    use ic_model::{HostState, ServiceState};

    use super::*;

    #[test]
    fn paint_resolves_states_through_the_theme() {
        let theme = Theme::dark();
        let critical = Paint::from(CheckableState::Service(ServiceState::Critical));
        assert_eq!(critical.resolve(&theme), theme.states.critical);
        let down = Paint::from(CheckableState::Host(HostState::Down));
        assert_eq!(down.resolve(&theme), theme.states.critical);
        let custom = Paint::from(theme.colors.accent);
        assert_eq!(custom.resolve(&theme), theme.colors.accent);
    }

    #[test]
    fn circle_sizes_follow_the_design() {
        let metrics = Metrics::default();
        assert_eq!(CircleSize::Row.dimensions(&metrics), (px(22.), px(3.)));
        assert_eq!(CircleSize::Compact.dimensions(&metrics), (px(14.), px(2.)));
        assert_eq!(CircleSize::Pane.dimensions(&metrics).0, px(34.));
    }

    #[test]
    fn builders_record_their_options() {
        let circle = StateCircle::new(CheckableState::Service(ServiceState::Warning))
            .size(CircleSize::Compact)
            .handled(true)
            .caption("9m");
        assert!(circle.handled);
        assert_eq!(circle.size, CircleSize::Compact);
        assert!(matches!(circle.caption, Some(Caption::Muted(ref text)) if text == "9m"));

        let labelled = StateCircle::with_color(gpui::red()).state_label();
        assert_eq!(labelled.state, None, "custom colours have no state label");
    }
}
