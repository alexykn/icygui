//! The notification centre (NOTE-05): opened from the footer's clock
//! icon, above it. It lists the recent notifications from the local log
//! and every new one, silent ones too, newest first: unread ones bright
//! with a dot, read ones muted. A click opens the object and marks the
//! entry read; *mark all read* clears the badge. The header pauses
//! notifications (30 minutes, an hour, until 08:00) or resumes them, and
//! the footer opens the notification settings.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement as _, IntoElement, MouseDownEvent,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px,
};
use gpui::{BoxShadow, point};
use ic_model::Timestamp;
use ic_rules::Tone;
use ic_ui_kit::{ActiveTheme as _, Chip, Link, StateDot, Theme, Tooltip};

use super::{Sidebar, SidebarEvent, SidebarMenu};
use crate::notifications::entry::{self, CentreEntry, SILENT_HINT};
use crate::notifications::{PauseChoice, when};
use crate::settings::SettingsTab;

/// The centre's width.
const CENTRE_WIDTH: f32 = 420.;
/// The list's height before it scrolls.
const LIST_MAX_HEIGHT: f32 = 440.;

/// The colour of a notification's tone.
pub(crate) fn tone_color(tone: Tone, theme: &Theme) -> gpui::Hsla {
    match tone {
        Tone::Critical => theme.states.critical,
        Tone::Warning => theme.states.warning,
        Tone::Unknown => theme.states.unknown,
        Tone::Recovery => theme.states.ok,
        Tone::Info => theme.colors.accent,
    }
}

impl Sidebar {
    /// Opens the notification centre (the palette's *Notifications*).
    pub(crate) fn open_notifications(&mut self, cx: &mut Context<Self>) {
        self.menus.open(SidebarMenu::Notifications);
        cx.notify();
    }

    /// Whether the notification centre is open.
    #[cfg(all(test, target_os = "linux"))]
    pub(crate) fn notifications_open(&self) -> bool {
        self.menus.is_open(&SidebarMenu::Notifications)
    }

    /// The notification centre's card.
    pub(super) fn notification_centre(&self, now: Timestamp, cx: &Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let entries = entry::entries(state.notification_records(), now);
        let has_environment = state.environment().is_some();
        let header = Self::centre_header(&entries, theme, cx);
        let pause_row = has_environment.then(|| self.centre_pause(now, theme, cx));
        let list = Self::centre_list(&entries, theme, cx);
        let footer = Self::centre_footer(has_environment, theme, cx);
        div()
            .id("notification-centre")
            .occlude()
            .flex()
            .flex_col()
            .w(px(CENTRE_WIDTH))
            .rounded(theme.metrics.code_radius)
            .border_1()
            .border_color(colors.border_window)
            .bg(colors.element_background)
            .shadow(vec![BoxShadow {
                color: colors.shadow_strong,
                offset: point(px(0.), px(6.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            }])
            .on_mouse_down_out(cx.listener(|this, event: &MouseDownEvent, _, cx| {
                this.menus.dismiss(event.position);
                cx.notify();
            }))
            .child(header)
            .children(pause_row)
            .child(list)
            .child(footer)
            .into_any_element()
    }

    /// `Notifications · 3 unread` and *mark all read*.
    fn centre_header(entries: &[CentreEntry], theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let unread = entries.iter().filter(|entry| entry.unread).count();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(40.))
            .px(px(14.))
            .border_b_1()
            .border_color(colors.border_header)
            .child(
                div()
                    .text_size(theme.text.heading)
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(colors.text_strong)
                    .child("Notifications"),
            )
            .child(
                div()
                    .text_size(theme.text.small)
                    .text_color(colors.text_faint)
                    .child(entry::summary(unread, entries.len())),
            )
            .child(div().flex_1())
            .when(unread > 0, |header| {
                header.child(
                    Link::new("centre-mark-all-read", "mark all read")
                        .quiet()
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                if state.mark_all_notifications_read() {
                                    cx.notify();
                                }
                            });
                        })),
                )
            })
            .into_any_element()
    }

    /// Pausing (30 minutes, an hour, until 08:00), or resuming.
    fn centre_pause(&self, now: Timestamp, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let paused = self
            .state
            .read(cx)
            .paused_until()
            .filter(|until| *until > now);
        let row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .px(px(14.))
            .py(px(8.))
            .border_b_1()
            .border_color(colors.border_row)
            .text_size(theme.text.small);
        match paused {
            Some(until) => row
                .child(
                    div()
                        .flex_1()
                        .text_color(theme.states.warning)
                        .child(format!("paused until {}: shown silently", when(until, now))),
                )
                .child(
                    Link::new("centre-resume", "resume")
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                state.pause_notifications(None);
                                cx.notify();
                            });
                        })),
                )
                .into_any_element(),
            None => row
                .child(div().flex_1().text_color(colors.text_faint).child("pause"))
                .children(PauseChoice::ALL.map(|choice| {
                    Chip::new(
                        SharedString::from(format!("centre-pause-{choice:?}")),
                        choice.short_label(),
                    )
                    .on_click(cx.listener(
                        move |this, _: &ClickEvent, _, cx| {
                            this.state.update(cx, |state, cx| {
                                state.pause_notifications(Some(choice.until(Timestamp::now())));
                                cx.notify();
                            });
                        },
                    ))
                }))
                .into_any_element(),
        }
    }

    /// The entries, newest first, or why there are none.
    fn centre_list(entries: &[CentreEntry], theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        if entries.is_empty() {
            return div()
                .flex()
                .flex_col()
                .gap(px(6.))
                .px(px(14.))
                .py(px(18.))
                .text_size(theme.text.small)
                .child(
                    div()
                        .text_color(colors.text_muted)
                        .child("No notifications yet."),
                )
                .child(div().text_color(colors.text_faint).child(
                    "They appear here as they happen, silent ones too: quiet hours, pauses \
                     and storms are recorded without a system notification.",
                ))
                .into_any_element();
        }
        div()
            .id("centre-list")
            .flex()
            .flex_col()
            .max_h(px(LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .py(px(4.))
            .children(
                entries
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| Self::centre_row(index, entry, theme, cx)),
            )
            .into_any_element()
    }

    /// Where it all stays, and the settings.
    fn centre_footer(has_environment: bool, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let colors = theme.colors;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(36.))
            .px(px(14.))
            .border_t_1()
            .border_color(colors.border_header)
            .text_size(theme.text.small)
            .text_color(colors.text_faint)
            .child(div().flex_1().child("only on this computer"))
            .when(has_environment, |footer| {
                footer.child(
                    Link::new("centre-settings", "notification settings…")
                        .quiet()
                        .text_size(theme.text.small)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.menus.close();
                            cx.emit(SidebarEvent::OpenSettings(SettingsTab::Notifications));
                            cx.notify();
                        })),
                )
            })
            .into_any_element()
    }

    /// One entry: time, tone dot, title, first line, where it matched.
    fn centre_row(
        index: usize,
        entry: &CentreEntry,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let tone = tone_color(entry.tone, theme);
        let silent = entry.detail.ends_with("silent");
        let id = entry.id.clone();
        let object = entry.object.clone();
        let row = div()
            .id(SharedString::from(format!("centre-entry-{index}")))
            .flex()
            .items_start()
            .gap(px(10.))
            .px(px(14.))
            .py(px(7.))
            .cursor_pointer()
            .hover(|style| style.bg(colors.row_hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.state.update(cx, |state, cx| {
                    if state.mark_notification_read(&id) {
                        cx.notify();
                    }
                });
                if let Some(object) = &object {
                    this.menus.close();
                    cx.emit(SidebarEvent::OpenObject(object.clone()));
                }
                cx.notify();
            }))
            .child(
                div()
                    .w(px(44.))
                    .flex_none()
                    .pt(px(1.))
                    .text_size(theme.text.hint)
                    .text_color(colors.text_faint)
                    .child(entry.time.clone()),
            )
            .child(
                div()
                    .pt(px(5.))
                    .flex_none()
                    .child(StateDot::with_color(tone).size(px(7.))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(1.))
                    .child(
                        div()
                            .truncate()
                            .text_size(theme.text.body)
                            .text_color(if entry.unread {
                                colors.text_strong
                            } else {
                                colors.text_muted
                            })
                            .child(entry.title.clone()),
                    )
                    .when(!entry.body.is_empty(), |column| {
                        column.child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_faint)
                                .child(entry.body.clone()),
                        )
                    })
                    .when(!entry.detail.is_empty(), |column| {
                        column.child(
                            div()
                                .truncate()
                                .text_size(theme.text.hint)
                                .text_color(colors.text_faint)
                                .child(entry.detail.clone()),
                        )
                    }),
            )
            .child(
                div()
                    .w(px(6.))
                    .h(px(6.))
                    .mt(px(6.))
                    .flex_none()
                    .rounded_full()
                    .when(entry.unread, |dot| dot.bg(colors.accent)),
            );
        if silent {
            row.tooltip(Tooltip::text(SILENT_HINT)).into_any_element()
        } else {
            row.into_any_element()
        }
    }
}
