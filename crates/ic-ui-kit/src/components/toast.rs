//! Toasts: small cards in a corner of the window that report what became
//! of something the user did (an action sent to Icinga), with a line per
//! object it failed for.

use std::fmt;

use gpui::{
    AnyElement, App, BoxShadow, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, point, prelude::FluentBuilder as _, px, relative,
};

use crate::components::IconButton;
use crate::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, Theme};

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// The width of a toast.
pub const TOAST_WIDTH: f32 = 380.;

/// What a [`Toast`] reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ToastTone {
    /// Under way: the accent colour and a loader.
    #[default]
    Pending,
    /// Done: the OK colour and a check mark.
    Success,
    /// Done in part, or something to look at: the warning colour.
    Warning,
    /// Failed: the critical colour.
    Critical,
    /// For information: the accent colour.
    Info,
}

impl ToastTone {
    /// The tone's colour.
    #[must_use]
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Pending | Self::Info => theme.colors.accent,
            Self::Success => theme.states.ok,
            Self::Warning => theme.states.warning,
            Self::Critical => theme.states.critical,
        }
    }

    /// The tone's icon.
    #[must_use]
    pub fn icon(self) -> IconName {
        match self {
            Self::Pending => IconName::Loader,
            Self::Success => IconName::Check,
            Self::Warning | Self::Critical => IconName::TriangleAlert,
            Self::Info => IconName::Info,
        }
    }
}

/// A card reporting one thing:
///
/// ```text
/// ┃ ⚠  Acknowledged 2 of 3 services                    ×
/// ┃    load on db-01: Service db-01!load is OK.
/// ┃    + 2 more
/// ```
///
/// Children added with [`ParentElement`] go under the lines (links).
#[derive(IntoElement)]
#[must_use = "a toast does nothing unless rendered"]
pub struct Toast {
    id: ElementId,
    tone: ToastTone,
    title: SharedString,
    lines: Vec<SharedString>,
    footer: Option<SharedString>,
    actions: Vec<AnyElement>,
    on_dismiss: Option<ClickHandler>,
}

impl Toast {
    /// A toast saying `title`.
    pub fn new(id: impl Into<ElementId>, tone: ToastTone, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            tone,
            title: title.into(),
            lines: Vec::new(),
            footer: None,
            actions: Vec::new(),
            on_dismiss: None,
        }
    }

    /// Adds a line under the title (a failure, a detail).
    pub fn line(mut self, line: impl Into<SharedString>) -> Self {
        self.lines.push(line.into());
        self
    }

    /// A faint last line (`+ 3 more`).
    pub fn footer(mut self, footer: impl Into<SharedString>) -> Self {
        self.footer = Some(footer.into());
        self
    }

    /// Shows a × that runs `handler`.
    pub fn on_dismiss(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_dismiss = Some(Box::new(handler));
        self
    }

    /// The title.
    #[must_use]
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    /// The lines under the title.
    #[must_use]
    pub fn lines(&self) -> &[SharedString] {
        &self.lines
    }
}

impl ParentElement for Toast {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.actions.extend(elements);
    }
}

impl fmt::Debug for Toast {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Toast")
            .field("id", &self.id)
            .field("tone", &self.tone)
            .field("title", &self.title)
            .field("lines", &self.lines)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Toast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let tone = self.tone.color(theme);
        let dismiss_id = ElementId::Name(format!("{}-dismiss", self.id).into());
        div()
            .id(self.id)
            .role(Role::Status)
            .aria_label(self.title.clone())
            .occlude()
            .relative()
            .flex()
            .items_start()
            .gap(px(10.))
            .w(px(TOAST_WIDTH))
            .py(px(10.))
            .pl(px(14.))
            .pr(px(8.))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.window_background)
            .overflow_hidden()
            .shadow(vec![BoxShadow {
                color: colors.shadow_strong,
                offset: point(px(0.), px(8.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
                inset: false,
            }])
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(tone),
            )
            .child(
                div().flex_none().pt(px(2.)).child(
                    Icon::new(self.tone.icon())
                        .size(theme.metrics.icon)
                        .color(tone),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(4.))
                    .child(
                        div()
                            .text_size(theme.text.body)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text_strong)
                            .line_height(relative(1.4))
                            .child(self.title),
                    )
                    .children(self.lines.into_iter().map(|line| {
                        div()
                            .text_size(theme.text.small)
                            .text_color(colors.text_muted)
                            .line_height(relative(1.45))
                            .child(line)
                    }))
                    .when_some(self.footer, |column, footer| {
                        column.child(
                            div()
                                .text_size(theme.text.small)
                                .text_color(colors.text_faint)
                                .child(footer),
                        )
                    })
                    .when(!self.actions.is_empty(), |column| {
                        column.child(
                            div()
                                .flex()
                                .gap(px(14.))
                                .pt(px(2.))
                                .text_size(theme.text.small)
                                .children(self.actions),
                        )
                    }),
            )
            .when_some(self.on_dismiss, |toast, handler| {
                toast.child(
                    IconButton::new(dismiss_id, IconName::Close)
                        .size(px(20.))
                        .icon_size(px(12.))
                        .color(colors.text_faint)
                        .on_click(handler),
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toasts_collect_lines() {
        let toast = Toast::new("t", ToastTone::Warning, "Acknowledged 2 of 3 services")
            .line("load on db-01: is OK")
            .footer("+ 1 more");
        assert_eq!(toast.title(), "Acknowledged 2 of 3 services");
        assert_eq!(toast.lines().len(), 1);
        assert!(format!("{toast:?}").contains("Warning"));
    }

    #[test]
    fn tones_have_colours_and_icons() {
        let theme = Theme::dark();
        assert_eq!(ToastTone::Success.color(&theme), theme.states.ok);
        assert_eq!(ToastTone::Critical.color(&theme), theme.states.critical);
        assert_eq!(ToastTone::Pending.icon(), IconName::Loader);
        assert_eq!(ToastTone::Warning.icon(), IconName::TriangleAlert);
    }
}
