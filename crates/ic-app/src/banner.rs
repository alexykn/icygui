//! The banners over the main area (ENV-07): the connection's problems
//! (reconnecting with a countdown and "Retry now", login refused,
//! certificate not trusted, password missing, settings that can't work)
//! and settings that couldn't be saved; and the same problems as the whole
//! body while there is nothing else to show (UI-05).

use std::time::Instant;

use gpui::{
    AnyElement, App, ClickEvent, ElementId, Entity, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};
use ic_model::Timestamp;
use ic_ui_kit::{
    ActiveTheme as _, Banner, BannerTone, Button, EmptyState, Icon, IconName, Link, ProgressBar,
};

use crate::actions::{EditEnvironment, ReviewCertificate};
use crate::app_state::{AppState, ConnectionNotice, NoticeAction, NoticeKind, Tone};

/// The banners to show over a list or tab at `now`.
pub(crate) fn banners(state: &Entity<AppState>, now: Timestamp, cx: &App) -> Vec<AnyElement> {
    let current = state.read(cx);
    let mut banners = Vec::new();
    if let Some(notice) = current.connection_notice(now) {
        banners.push(connection_banner(state, &notice).into_any_element());
    }
    if let Some(error) = current.save_error() {
        let dismiss = state.clone();
        banners.push(
            Banner::new(
                "save-error",
                BannerTone::Warning,
                "The settings couldn't be saved.",
            )
            .detail(error.to_owned())
            .child(
                Link::new("dismiss-save-error", "Dismiss")
                    .quiet()
                    .on_click(move |_, _, cx| {
                        dismiss.update(cx, |state, cx| {
                            state.dismiss_save_error();
                            cx.notify();
                        });
                    }),
            )
            .into_any_element(),
        );
    }
    if let Some(notice) = current.notice() {
        let dismiss = state.clone();
        let tone = if notice.problem {
            BannerTone::Warning
        } else {
            BannerTone::Info
        };
        let mut banner =
            Banner::new("user-notice", tone, notice.title.clone()).icon(if notice.problem {
                IconName::TriangleAlert
            } else {
                IconName::Info
            });
        if let Some(detail) = &notice.detail {
            banner = banner.detail(detail.clone());
        }
        banners.push(
            banner
                .child(
                    Link::new("dismiss-notice", "Dismiss")
                        .quiet()
                        .on_click(move |_, _, cx| {
                            dismiss.update(cx, |state, cx| {
                                state.dismiss_notice();
                                cx.notify();
                            });
                        }),
                )
                .into_any_element(),
        );
    }
    banners
}

/// The load progress bar while connecting or loading.
pub(crate) fn progress(state: &AppState) -> Option<AnyElement> {
    state
        .connection()
        .progress()
        .map(|progress| ProgressBar::new(progress.fraction).into_any_element())
}

fn icon(kind: NoticeKind) -> IconName {
    match kind {
        NoticeKind::Reconnecting => IconName::Unplug,
        NoticeKind::AuthFailed | NoticeKind::MissingSecret => IconName::KeyRound,
        NoticeKind::TlsFailed => IconName::Lock,
        NoticeKind::Misconfigured | NoticeKind::EngineFailed => IconName::TriangleAlert,
    }
}

fn tone(tone: Tone) -> BannerTone {
    match tone {
        Tone::Critical => BannerTone::Critical,
        Tone::Warning => BannerTone::Warning,
    }
}

/// What a notice's action does when clicked.
fn on_action(
    state: &Entity<AppState>,
    action: NoticeAction,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) + 'static {
    let state = state.clone();
    move |_, window, cx| match action {
        NoticeAction::RetryNow => state.update(cx, |state, cx| {
            if state.refresh(Instant::now()) {
                cx.notify();
            }
        }),
        NoticeAction::ReviewCertificate => {
            window.dispatch_action(Box::new(ReviewCertificate), cx);
        }
        NoticeAction::EditEnvironment => window.dispatch_action(Box::new(EditEnvironment), cx),
    }
}

fn action_id(prefix: &str, action: NoticeAction) -> ElementId {
    let name = match action {
        NoticeAction::RetryNow => "retry",
        NoticeAction::ReviewCertificate => "review-certificate",
        NoticeAction::EditEnvironment => "edit-environment",
    };
    ElementId::Name(SharedString::from(format!("{prefix}-{name}")))
}

/// A connection notice as a banner, its actions as links.
fn connection_banner(state: &Entity<AppState>, notice: &ConnectionNotice) -> Banner {
    let mut banner = Banner::new("connection-banner", tone(notice.tone), notice.title.clone())
        .icon(icon(notice.kind));
    if let Some(detail) = &notice.detail {
        banner = banner.detail(detail.clone());
    }
    for action in &notice.actions {
        let link = Link::new(action_id("banner", *action), action.label())
            .on_click(on_action(state, *action));
        banner = banner.child(link);
    }
    banner
}

/// A connection notice as the whole body, its actions as buttons (before
/// anything was loaded).
pub(crate) fn connection_body(
    state: &Entity<AppState>,
    notice: &ConnectionNotice,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let color = match notice.tone {
        Tone::Critical => theme.states.critical,
        Tone::Warning => theme.states.warning,
    };
    let mut buttons = div().flex().flex_wrap().justify_center().gap(px(8.));
    for (index, action) in notice.actions.iter().enumerate() {
        let mut button = Button::new(
            action_id("body", *action),
            action.label().trim_end_matches('…'),
        )
        .on_click(on_action(state, *action));
        if index == 0 {
            button = button.primary();
        }
        buttons = buttons.child(button);
    }
    let mut body = EmptyState::new(notice.title.clone())
        .leading(Icon::new(icon(notice.kind)).size(px(22.)).color(color))
        .max_width(px(620.));
    if let Some(detail) = &notice.detail {
        body = body.detail(detail.clone());
    }
    body.child(buttons).into_any_element()
}

/// The body while connecting or loading, before anything can be shown:
/// what is happening and how far it got.
pub(crate) fn loading_body(state: &AppState, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let progress = state.connection().progress();
    let text = progress.as_ref().map_or_else(
        || "Connecting…".to_owned(),
        |progress| progress.text.clone(),
    );
    let fraction = progress.map_or(0., |progress| progress.fraction);
    let name = state
        .environment()
        .map_or_else(String::new, |environment| environment.name.clone());
    EmptyState::new(if name.is_empty() {
        "Loading…".to_owned()
    } else {
        format!("Loading {name}")
    })
    .leading(
        Icon::new(IconName::Loader)
            .size(px(22.))
            .color(theme.colors.accent),
    )
    .detail(text)
    .child(
        div()
            .w(px(320.))
            .child(ProgressBar::new(fraction).height(px(3.))),
    )
    .into_any_element()
}
