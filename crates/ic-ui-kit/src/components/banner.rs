//! Status strips for the main area: the connection banner (reconnecting,
//! login refused, certificate not trusted, …) and the thin load progress
//! bar under a header.

use std::fmt;

use gpui::{
    AnyElement, App, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, prelude::FluentBuilder as _, px, relative,
};

use crate::components::Tooltip;
use crate::icon::{Icon, IconName};
use crate::theme::{ActiveTheme as _, Theme};

/// Width of the tone bar at a banner's left edge.
const TONE_BAR: f32 = 2.;

/// How urgent a [`Banner`] is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BannerTone {
    /// Something is broken and needs attention (connection lost, login
    /// refused): the critical colour.
    Critical,
    /// Something needs attention but works (a stale connection, settings
    /// that couldn't be saved): the warning colour.
    Warning,
    /// For information (loading, connecting): the accent colour.
    #[default]
    Info,
}

impl BannerTone {
    /// The tone's colour.
    #[must_use]
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Critical => theme.states.critical,
            Self::Warning => theme.states.warning,
            Self::Info => theme.colors.accent,
        }
    }
}

/// A full-width strip under a header that says what is wrong with the
/// connection (or the settings) and offers what to do about it:
///
/// ```text
/// ▌ ⚠  Connection to master-01 lost. Retrying in 12s.     Retry now
///      connect: connection refused (127.0.0.1:5665)
/// ```
///
/// The background is the tone's colour, faintly; the tone bar and icon are
/// the full colour. Children added with [`ParentElement`] go at the right
/// end (links or buttons).
#[derive(IntoElement)]
#[must_use = "a banner does nothing unless rendered"]
pub struct Banner {
    id: ElementId,
    tone: BannerTone,
    icon: IconName,
    title: SharedString,
    detail: Option<SharedString>,
    detail_tooltip: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl Banner {
    /// A banner saying `title`, with a warning triangle.
    pub fn new(id: impl Into<ElementId>, tone: BannerTone, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            tone,
            icon: IconName::TriangleAlert,
            title: title.into(),
            detail: None,
            detail_tooltip: None,
            actions: Vec::new(),
        }
    }

    /// Replaces the icon.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    /// A second, muted line (the error Icinga or the network reported). It
    /// is cut to one line; the full text shows in a tooltip.
    pub fn detail(mut self, detail: impl Into<SharedString>) -> Self {
        let detail = detail.into();
        self.detail_tooltip = Some(detail.clone());
        self.detail = Some(detail);
        self
    }

    /// The banner's tone.
    #[must_use]
    pub fn tone(&self) -> BannerTone {
        self.tone
    }

    /// The title.
    #[must_use]
    pub fn title(&self) -> &SharedString {
        &self.title
    }
}

impl ParentElement for Banner {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.actions.extend(elements);
    }
}

impl fmt::Debug for Banner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Banner")
            .field("id", &self.id)
            .field("tone", &self.tone)
            .field("title", &self.title)
            .field("detail", &self.detail)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for Banner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let tone = self.tone.color(theme);
        let detail_id = ElementId::Name(format!("{}-detail", self.id).into());
        div()
            .id(self.id)
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .w_full()
            .min_h(px(36.))
            .py(px(8.))
            .pl(theme.metrics.list_padding)
            .pr(theme.metrics.list_padding)
            .bg(tone.opacity(0.08))
            .border_b_1()
            .border_color(colors.border_header)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(TONE_BAR))
                    .bg(tone),
            )
            .child(Icon::new(self.icon).size(theme.metrics.icon).color(tone))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(theme.text.body)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors.text_strong)
                            .line_height(relative(1.4))
                            .child(self.title),
                    )
                    .when_some(self.detail, |column, detail| {
                        let tooltip = self.detail_tooltip.unwrap_or_default();
                        column.child(
                            div()
                                .id(detail_id)
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(detail)
                                .tooltip(Tooltip::text(tooltip)),
                        )
                    }),
            )
            .when(!self.actions.is_empty(), |banner| {
                banner.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(px(14.))
                        .text_size(theme.text.small)
                        .children(self.actions),
                )
            })
    }
}

/// A thin bar showing how far a load has got, under a header:
/// `ProgressBar::new(0.4)`.
#[derive(IntoElement)]
#[must_use = "a progress bar does nothing unless rendered"]
pub struct ProgressBar {
    fraction: f32,
    height: Pixels,
}

impl ProgressBar {
    /// A bar `fraction` full (clamped to 0–1; not a number counts as 0).
    pub fn new(fraction: f32) -> Self {
        Self {
            fraction: Self::clamp(fraction),
            height: px(2.),
        }
    }

    /// Sets the bar's height (default 2px).
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// The fraction drawn.
    #[must_use]
    pub fn fraction(&self) -> f32 {
        self.fraction
    }

    fn clamp(fraction: f32) -> f32 {
        if fraction.is_nan() {
            0.
        } else {
            fraction.clamp(0., 1.)
        }
    }
}

impl fmt::Debug for ProgressBar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProgressBar")
            .field("fraction", &self.fraction)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .flex_none()
            .w_full()
            .h(self.height)
            .bg(theme.colors.border_header)
            .child(
                div()
                    .h_full()
                    .w(relative(self.fraction))
                    .bg(theme.colors.accent),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_clamped() {
        assert!((ProgressBar::new(0.4).fraction() - 0.4).abs() < f32::EPSILON);
        assert!(ProgressBar::new(-1.).fraction().abs() < f32::EPSILON);
        assert!((ProgressBar::new(7.).fraction() - 1.).abs() < f32::EPSILON);
        assert!(ProgressBar::new(f32::NAN).fraction().abs() < f32::EPSILON);
    }

    #[test]
    fn tones_use_the_state_colours() {
        let theme = Theme::dark();
        assert_eq!(BannerTone::Critical.color(&theme), theme.states.critical);
        assert_eq!(BannerTone::Warning.color(&theme), theme.states.warning);
        assert_eq!(BannerTone::Info.color(&theme), theme.colors.accent);
    }

    #[test]
    fn banners_record_their_parts() {
        let banner = Banner::new("connection", BannerTone::Critical, "Connection lost")
            .icon(IconName::Unplug)
            .detail("connection refused");
        assert_eq!(banner.tone(), BannerTone::Critical);
        assert_eq!(banner.title(), "Connection lost");
        assert!(format!("{banner:?}").contains("connection refused"));
    }
}
