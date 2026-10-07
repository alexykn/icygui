//! A pane's downtimes (topic 01, variant A): the banner fixed under the
//! pane's header, between the header and the scrolling body, and the
//! `other downtimes` section where the comments are.
//!
//! What they say comes from [`crate::downtimes`]; this draws it. The
//! banner shows the downtime in effect (else the next to begin) in the
//! accent while it is in effect and grey before, with *remove downtime*
//! (asks first, listing every downtime it removes; disabled with the
//! reason for a downtime from the config or without the permission). When
//! a downtime starts while the pane is open, the banner comes in and the
//! body moves down once (accepted in the review); nothing else moves.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div,
};
use ic_core::snapshot::Snapshot;
use ic_model::{ObjectKey, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, Button, Icon, IconButton, IconName, Link, PaneBanner, PaneBannerTone,
    SectionLabel, Theme, Tooltip, px,
};

use super::ObjectPane;
use crate::actions::ObjectAction;
use crate::downtimes::{self, Fact};

/// The hover group of an other downtime (reveals its remove button).
const OTHER_GROUP: &str = "pane-other-downtime";

/// The banner for the pane's object, if it has a downtime in effect or
/// still to come.
pub(super) fn banner(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    now: Timestamp,
    content_width: Option<f32>,
    cx: &Context<ObjectPane>,
) -> Option<AnyElement> {
    let banner = downtimes::banner(snapshot, &pane.object, now)?;
    let theme = cx.theme();
    let tone = if banner.in_effect {
        PaneBannerTone::Active
    } else {
        PaneBannerTone::Quiet
    };
    let facts: Vec<AnyElement> = banner
        .facts
        .iter()
        .map(|fact| match fact {
            Fact::Text(text) => div().flex_none().child(text.clone()).into_any_element(),
            Fact::Host(host) => {
                let target = ObjectKey::host(host);
                div()
                    .flex()
                    .flex_none()
                    .child("host\u{a0}")
                    .child(
                        Link::new("downtime-host", host.clone())
                            .tooltip(Tooltip::new("Show the host and its downtime"))
                            .on_click(cx.listener(
                                move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                                    pane.navigate(target.clone(), cx);
                                },
                            )),
                    )
                    .into_any_element()
            }
        })
        .collect();
    let mut element = PaneBanner::new("downtime-banner", tone, banner.title)
        .icon(if banner.config {
            IconName::Lock
        } else {
            IconName::CalendarClock
        })
        .status(banner.status.clone())
        .action(remove_button(pane, &banner, cx))
        .facts(PaneBanner::facts_line(facts, theme))
        .note(
            banner.author.clone(),
            banner.entered.clone(),
            banner.comment.clone(),
        )
        .progress(banner.progress);
    // A tab keeps its body to a readable width: so do the banner's lines,
    // and *remove downtime* ends where the body ends.
    if let Some(width) = content_width {
        element = element.content_width(px(width));
    }
    if !banner.more.is_empty() {
        element = element.more(banner.more.join(" · "));
    }
    Some(element.into_any_element())
}

/// *remove downtime*: asks first, listing every downtime it removes.
/// Disabled with the reason for a downtime from the config (Icinga
/// refuses to remove those) or when the API user may not remove downtimes.
fn remove_button(
    pane: &ObjectPane,
    banner: &downtimes::Banner,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let action = ObjectAction::RemoveDowntime(banner.name.clone());
    let button = Button::new("remove-downtime", "remove downtime");
    if banner.config {
        return button
            .disabled(true)
            .tooltip(Tooltip::new(config_reason(banner.schedule.as_deref())))
            .into_any_element();
    }
    match pane.state.read(cx).action_denial(&action) {
        Some(denial) => button
            .disabled(true)
            .tooltip(Tooltip::new(denial))
            .into_any_element(),
        None => button
            .on_click(
                cx.listener(move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                    pane.request(action.clone(), cx);
                }),
            )
            .into_any_element(),
    }
}

/// Why a downtime from the config can't be removed.
pub(super) fn config_reason(schedule: Option<&str>) -> String {
    let from = schedule.map_or_else(
        || "a ScheduledDowntime".to_owned(),
        |schedule| format!("the ScheduledDowntime {schedule}"),
    );
    format!("From the config ({from}): Icinga refuses to remove it, and the config brings it back")
}

/// The object's other downtimes (the banner shows one), each with its
/// window, fixed or flexible, status, author and comment, and a remove
/// `×` in a fixed slot that shows on hover. A downtime from the config
/// shows a lock, and its `×` says why it can't be removed.
pub(super) fn others(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    object: &ObjectKey,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> Option<AnyElement> {
    let others = downtimes::others(snapshot, object, now);
    if others.is_empty() {
        return None;
    }
    let theme = cx.theme();
    let state = pane.state.read(cx);
    let mut column = div()
        .flex()
        .flex_col()
        .gap(px(14.))
        .child(SectionLabel::new("other downtimes"));
    for other in others {
        let action = ObjectAction::RemoveDowntime(other.name.clone());
        let remove = IconButton::new(
            SharedString::from(format!("remove-downtime-{}", other.name)),
            IconName::Close,
        )
        .size(px(20.))
        .icon_size(px(12.))
        .color(theme.colors.text_faint);
        let (remove, enabled) = if other.config {
            (
                remove
                    .disabled(true)
                    .tooltip(Tooltip::new(config_reason(None))),
                false,
            )
        } else {
            match state.action_denial(&action) {
                Some(denial) => (remove.disabled(true).tooltip(Tooltip::new(denial)), false),
                None => (
                    remove
                        .tooltip(Tooltip::new("Remove downtime"))
                        .on_click(cx.listener(
                            move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                                pane.request(action.clone(), cx);
                            },
                        )),
                    true,
                ),
            }
        };
        column = column.child(other_entry(&other, remove, enabled, theme));
    }
    Some(column.into_any_element())
}

/// One other downtime: icon, the facts line, `author time comment`, the
/// remove slot. Hovering the entry shows its `×` as a small filled button
/// (as drawn); a config downtime's stays plain and faint.
fn other_entry(
    other: &downtimes::Other,
    remove: IconButton,
    enabled: bool,
    theme: &Theme,
) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .id(SharedString::from(format!("other-downtime-{}", other.name)))
        .group(OTHER_GROUP)
        .flex()
        .items_start()
        .gap(px(10.))
        .text_size(theme.text.body)
        .child(
            div().flex_none().w(px(14.)).pt(px(2.)).child(
                Icon::new(if other.config {
                    IconName::Lock
                } else {
                    IconName::CalendarClock
                })
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
                .gap(px(3.))
                .child(
                    div()
                        .truncate()
                        .text_color(colors.text)
                        .child(other.line.clone()),
                )
                .child(
                    div().text_color(colors.text_secondary).child(
                        gpui::StyledText::new(SharedString::from(format!(
                            "{} {} {}",
                            other.author, other.entered, other.comment
                        )))
                        .with_highlights(note_highlights(other, theme)),
                    ),
                ),
        )
        .child(
            div()
                .flex_none()
                .rounded(theme.metrics.small_radius)
                .invisible()
                .group_hover(OTHER_GROUP, |style| {
                    let style = style.visible();
                    if enabled {
                        style.bg(colors.element_hover)
                    } else {
                        style
                    }
                })
                .child(remove),
        )
}

/// The author in the text colour, the time faint, the comment as it is.
fn note_highlights(
    other: &downtimes::Other,
    theme: &Theme,
) -> Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> {
    let author = other.author.len();
    let entered = author + 1 + other.entered.len();
    vec![
        (
            0..author,
            gpui::HighlightStyle {
                color: Some(theme.colors.text),
                ..gpui::HighlightStyle::default()
            },
        ),
        (
            author..entered,
            gpui::HighlightStyle {
                color: Some(theme.colors.text_faint),
                ..gpui::HighlightStyle::default()
            },
        ),
    ]
}
