//! A pane's downtime banner (topic 01, variant A), fixed under the pane's
//! header, between the header and the scrolling body. The object's other
//! downtimes are entries of its thread ([`super::thread`]).
//!
//! What they say comes from [`crate::downtimes`]; this draws it. The
//! banner shows the downtime in effect (else the next to begin) in the
//! accent while it is in effect and grey before, with *remove downtime*
//! (asks first, listing every downtime it removes; disabled with the
//! reason for a downtime from the config or without the permission). When
//! a downtime starts while the pane is open, the banner comes in and the
//! body moves down once (accepted in the review); nothing else moves.

use gpui::{AnyElement, ClickEvent, Context, IntoElement, ParentElement as _, Styled as _, div};
use ic_core::snapshot::Snapshot;
use ic_model::{ObjectKey, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, Button, IconName, Link, PaneBanner, PaneBannerTone, Tooltip, px,
};

use super::ObjectPane;
use crate::actions::ObjectAction;
use crate::downtimes::{self, Fact};

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
