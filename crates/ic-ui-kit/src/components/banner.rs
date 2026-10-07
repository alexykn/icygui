//! Status strips for the main area: the connection banner (reconnecting,
//! login refused, certificate not trusted, …), the thin load progress bar
//! under a header, and the pane banner fixed under a pane's header (a
//! downtime, topic 01).

use std::fmt;

use crate::px;
use gpui::{
    AnyElement, App, Div, ElementId, FontWeight, HighlightStyle, Hsla, InteractiveElement as _,
    IntoElement, ParentElement, Pixels, RenderOnce, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, StyledText, Window, div,
    prelude::FluentBuilder as _, relative,
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
    /// The tone's colour, for the tone bar and the icon.
    #[must_use]
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::Critical => theme.states.fill.critical,
            Self::Warning => theme.states.fill.warning,
            Self::Info => theme.colors.accent,
        }
    }

    /// The tone's faint wash, for the banner's background.
    #[must_use]
    pub fn tint(self, theme: &Theme) -> Hsla {
        match self {
            Self::Critical => theme.colors.critical_tint,
            Self::Warning => theme.colors.warning_tint,
            Self::Info => theme.colors.accent_tint,
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
            .bg(self.tone.tint(theme))
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

/// How a [`PaneBanner`] looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum PaneBannerTone {
    /// In effect: the accent (the Info tone) with its progress.
    #[default]
    Active,
    /// Not in effect yet: grey, its progress line empty.
    Quiet,
}

/// A banner fixed under a pane's header, between the header and the
/// scrolling body, so it stays in view (topic 01, variant A: an object in
/// downtime). It is the [`Banner`] in the Info tone (the tint at 8 %, a 2px
/// tone bar) with more lines and a 2px [`ProgressBar`] on its bottom edge:
///
/// ```text
/// ▌ ◷ In downtime 1h 48m left                       [remove downtime]
///     fixed · 13:00 → 16:00 today · started 1h 12m ago
///     j.berg 12:41 Failover drill on db-prod-01: the standby on
///     db-prod-03 lags until the replica rebuild is done.
///     + 2 more below · tonight 22:00, flexible
/// ━━━━━━━━━━━━━━━━━━━━━━━━━──────────────────────────────────────────
/// ```
///
/// [`PaneBannerTone::Quiet`] is grey: scheduled but not in effect yet.
/// The note is two lines at most; the full text is in its tooltip.
#[derive(IntoElement)]
#[must_use = "a banner does nothing unless rendered"]
pub struct PaneBanner {
    id: ElementId,
    tone: PaneBannerTone,
    icon: IconName,
    title: SharedString,
    status: Option<SharedString>,
    action: Option<AnyElement>,
    facts: Option<AnyElement>,
    note: Option<(SharedString, SharedString, SharedString)>,
    more: Option<SharedString>,
    progress: f32,
}

impl PaneBanner {
    /// A banner saying `title` in `tone`, with a calendar.
    pub fn new(
        id: impl Into<ElementId>,
        tone: PaneBannerTone,
        title: impl Into<SharedString>,
    ) -> Self {
        Self {
            id: id.into(),
            tone,
            icon: IconName::CalendarClock,
            title: title.into(),
            status: None,
            action: None,
            facts: None,
            note: None,
            more: None,
            progress: 0.,
        }
    }

    /// Replaces the icon (a lock for a downtime from the config).
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = icon;
        self
    }

    /// Text after the title: how long is left (in the accent while in
    /// effect), or when it starts.
    pub fn status(mut self, status: impl Into<SharedString>) -> Self {
        self.status = Some(status.into());
        self
    }

    /// The element at the right of the first line (a button).
    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }

    /// The second line, the facts: one line, muted, cut where it doesn't
    /// fit. Build it with [`PaneBanner::facts_line`].
    pub fn facts(mut self, facts: impl IntoElement) -> Self {
        self.facts = Some(facts.into_any_element());
        self
    }

    /// The third line: who, when, why; two lines at most, the whole text
    /// in its tooltip.
    pub fn note(
        mut self,
        author: impl Into<SharedString>,
        at: impl Into<SharedString>,
        text: impl Into<SharedString>,
    ) -> Self {
        self.note = Some((author.into(), at.into(), text.into()));
        self
    }

    /// The last line, faint: what else there is.
    pub fn more(mut self, more: impl Into<SharedString>) -> Self {
        self.more = Some(more.into());
        self
    }

    /// How much has passed, 0 to 1 (the bottom edge's line).
    pub fn progress(mut self, fraction: f32) -> Self {
        self.progress = ProgressBar::new(fraction).fraction();
        self
    }

    /// The tone.
    #[must_use]
    pub fn tone(&self) -> PaneBannerTone {
        self.tone
    }

    /// The title.
    #[must_use]
    pub fn title(&self) -> &SharedString {
        &self.title
    }

    /// A facts line from its parts, with faint ` · ` between them: muted
    /// text, or elements such as links.
    pub fn facts_line(parts: Vec<AnyElement>, theme: &Theme) -> AnyElement {
        let mut line = div()
            .flex()
            .items_center()
            .min_w_0()
            .overflow_hidden()
            .whitespace_nowrap();
        for (index, part) in parts.into_iter().enumerate() {
            if index > 0 {
                line = line.child(
                    div()
                        .flex_none()
                        .text_color(theme.colors.text_faint)
                        .child("\u{a0}·\u{a0}"),
                );
            }
            line = line.child(part);
        }
        line.into_any_element()
    }
}

impl fmt::Debug for PaneBanner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PaneBanner")
            .field("id", &self.id)
            .field("tone", &self.tone)
            .field("title", &self.title)
            .field("status", &self.status)
            .field("more", &self.more)
            .finish_non_exhaustive()
    }
}

impl RenderOnce for PaneBanner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let active = self.tone == PaneBannerTone::Active;
        let (bar, icon, status, tint) = if active {
            (
                colors.accent,
                colors.accent,
                colors.accent_text,
                BannerTone::Info.tint(theme),
            )
        } else {
            (
                colors.text_faint,
                colors.text_muted,
                colors.text_secondary,
                colors.text_muted.opacity(0.07),
            )
        };
        let indent = px(ICON_SLOT + TITLE_GAP);
        let note_id = ElementId::Name(format!("{}-note", self.id).into());
        div()
            .id(self.id)
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(5.))
            .w_full()
            .pt(px(12.))
            .pb(px(13.))
            .pl(theme.metrics.pane_inset)
            .pr(theme.metrics.pane_padding)
            .bg(tint)
            .border_b_1()
            .border_color(colors.border_header)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(TONE_BAR))
                    .bg(bar),
            )
            .child(title_line(
                Icon::new(self.icon).size(px(ICON_SLOT)).color(icon),
                self.title,
                self.status.map(|text| div().text_color(status).child(text)),
                self.action,
                theme,
            ))
            .when_some(self.facts, |banner, facts| {
                banner.child(
                    div()
                        .pl(indent)
                        .min_w_0()
                        .text_size(theme.text.small)
                        .text_color(colors.text_muted)
                        .child(facts),
                )
            })
            .when_some(self.note, |banner, (author, at, text)| {
                banner.child(note_line(note_id, &author, &at, &text, theme).pl(indent))
            })
            .when_some(self.more, |banner, more| {
                banner.child(
                    div()
                        .pl(indent)
                        .truncate()
                        .text_size(theme.text.small)
                        .text_color(colors.text_faint)
                        .child(more),
                )
            })
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-1.))
                    .h(px(2.))
                    .bg(colors.border_header)
                    .when(active, |track| {
                        track.child(div().h_full().w(relative(self.progress)).bg(colors.accent))
                    }),
            )
    }
}

/// The pane banner's first line: icon, title, status, then the action at
/// the right.
fn title_line(
    icon: Icon,
    title: SharedString,
    status: Option<Div>,
    action: Option<AnyElement>,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(TITLE_GAP))
        .min_h(px(28.))
        .whitespace_nowrap()
        .child(div().flex().flex_none().w(px(ICON_SLOT)).child(icon))
        .child(
            div()
                .flex_none()
                .text_size(theme.text.row)
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.colors.text_strong)
                .child(title),
        )
        .when_some(status, |line, status| {
            line.child(status.flex_none().text_size(theme.text.row))
        })
        .child(div().flex_1())
        .children(action)
}

/// `author time comment`: the author in the text colour, the time faint,
/// at most two lines, the whole text in the tooltip.
fn note_line(id: ElementId, author: &str, at: &str, text: &str, theme: &Theme) -> Stateful<Div> {
    let colors = theme.colors;
    let full = format!("{author} {at} {text}");
    let author_end = author.len();
    let at_end = author_end + 1 + at.len();
    let styled = StyledText::new(SharedString::from(full.clone())).with_highlights([
        (
            0..author_end,
            HighlightStyle {
                color: Some(colors.text),
                ..HighlightStyle::default()
            },
        ),
        (
            author_end..at_end,
            HighlightStyle {
                color: Some(colors.text_faint),
                ..HighlightStyle::default()
            },
        ),
    ]);
    div()
        .id(id)
        .text_size(theme.text.body)
        .line_height(relative(1.45))
        .text_color(colors.text_secondary)
        .line_clamp(2)
        .child(styled)
        .tooltip(Tooltip::text(full))
}

/// The pane banner's icon slot: the icon's size.
const ICON_SLOT: f32 = 14.;
/// Space between the pane banner's icon, title and status.
const TITLE_GAP: f32 = 10.;

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
        assert_eq!(
            BannerTone::Critical.color(&theme),
            theme.states.fill.critical
        );
        assert_eq!(BannerTone::Warning.color(&theme), theme.states.fill.warning);
        assert_eq!(BannerTone::Warning.tint(&theme), theme.colors.warning_tint);
        assert_eq!(BannerTone::Info.color(&theme), theme.colors.accent);
    }

    #[test]
    fn pane_banners_record_their_parts() {
        let banner = PaneBanner::new("downtime", PaneBannerTone::Quiet, "Flexible downtime")
            .status("not started")
            .progress(3.);
        assert_eq!(banner.tone(), PaneBannerTone::Quiet);
        assert_eq!(banner.title(), "Flexible downtime");
        assert!((banner.progress - 1.).abs() < f32::EPSILON, "clamped");
        assert!(format!("{banner:?}").contains("not started"));
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
