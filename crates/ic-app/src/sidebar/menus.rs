//! The sidebar's popup menus: a group's and a dashboard's `···` (DASH-02,
//! DASH-03), the footer's `+` (new dashboard or group, import and export;
//! DASH-06), and the connection details with the environment switcher
//! (ENV-01, ENV-06).

use std::fmt::Write as _;
use std::time::Instant;

use gpui::{
    ClickEvent, Context, FontWeight, MouseDownEvent, ParentElement as _, Styled as _, div, px,
};
use ic_config::DashboardGroup;
use ic_model::{Timestamp, format_compact};
use ic_rules::{DashboardRef, ScopeSetting};
use ic_ui_kit::{ActiveTheme as _, Menu, MenuItem};

use super::{RenameTarget, Sidebar, SidebarEvent};
use crate::app_state::AppState;
use crate::settings::ScopeKey;

/// The notification settings a scope can take here (a custom rule is
/// edited in the notification settings).
const SCOPE_SETTINGS: [(&str, &str, ScopeSetting); 3] = [
    ("inherit", "inherit", ScopeSetting::Inherit),
    ("on", "on", ScopeSetting::On),
    ("off", "off (muted)", ScopeSetting::Off),
];

impl Sidebar {
    /// A listener that closes the open menu on a press outside it.
    fn dismiss_listener(
        cx: &Context<Self>,
    ) -> impl Fn(&MouseDownEvent, &mut gpui::Window, &mut gpui::App) + 'static {
        cx.listener(|this, event: &MouseDownEvent, _, cx| {
            this.menus.dismiss(event.position);
            cx.notify();
        })
    }

    /// A menu item that closes the menu and emits `event`.
    fn emit_item(
        id: impl Into<gpui::ElementId>,
        label: &'static str,
        event: SidebarEvent,
        cx: &Context<Self>,
    ) -> MenuItem {
        MenuItem::new(id, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menus.close();
            cx.emit(event.clone());
            cx.notify();
        }))
    }

    /// A menu item that closes the menu and changes the state with
    /// `change`.
    fn state_item(
        id: impl Into<gpui::ElementId>,
        label: impl Into<gpui::SharedString>,
        cx: &Context<Self>,
        change: impl Fn(&mut AppState) -> bool + 'static,
    ) -> MenuItem {
        MenuItem::new(id, label).on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
            this.menus.close();
            this.state.update(cx, |state, cx| {
                if change(state) {
                    cx.notify();
                }
            });
            cx.notify();
        }))
    }

    /// A group's `···` menu (DASH-02).
    pub(super) fn group_menu(
        group: &DashboardGroup,
        index: usize,
        group_count: usize,
        cx: &Context<Self>,
    ) -> Menu {
        let id = group.id.clone();
        let rename = RenameTarget::Group(id.clone());
        let mut menu = Menu::new("group-menu")
            .item(Self::emit_item(
                "group-new-dashboard",
                "new dashboard",
                SidebarEvent::NewDashboard(Some(id.clone())),
                cx,
            ))
            .item(
                MenuItem::new("group-rename", "rename").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.start_rename(rename.clone(), window, cx);
                    },
                )),
            )
            .item({
                let after = id.clone();
                Self::state_item("group-new-group", "new group", cx, move |state| {
                    state.create_group("", Some(&after)).is_some()
                })
            })
            .separator();
        let up = id.clone();
        let down = id.clone();
        let toggle = id.clone();
        menu = menu
            .item(
                Self::state_item("group-move-up", "move up", cx, move |state| {
                    state.move_group(&up, -1)
                })
                .disabled(index == 0),
            )
            .item(
                Self::state_item("group-move-down", "move down", cx, move |state| {
                    state.move_group(&down, 1)
                })
                .disabled(index + 1 >= group_count),
            )
            .item(Self::state_item(
                "group-collapse",
                if group.collapsed {
                    "expand"
                } else {
                    "collapse"
                },
                cx,
                move |state| state.toggle_group(&toggle),
            ))
            .separator()
            .label("notifications");
        for (key, label, setting) in SCOPE_SETTINGS {
            let target = id.clone();
            let checked = group.notifications == setting;
            menu = menu.item(
                Self::state_item(format!("group-notify-{key}"), label, cx, move |state| {
                    state.set_group_notifications(&target, setting.clone())
                })
                .checked(checked),
            );
        }
        menu = menu.item(
            Self::emit_item(
                "group-notify-custom",
                "custom rule…",
                SidebarEvent::CustomRule(ScopeKey::Group(id.clone())),
                cx,
            )
            .checked(matches!(group.notifications, ScopeSetting::Custom(_))),
        );
        menu.separator()
            .item(Self::emit_item(
                "group-export",
                "export group…",
                SidebarEvent::ExportGroups(vec![id.clone()]),
                cx,
            ))
            .item(Self::emit_item(
                "group-delete",
                "delete group…",
                SidebarEvent::DeleteGroup(id),
                cx,
            ))
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// A dashboard's `···` menu (DASH-03).
    pub(super) fn dashboard_menu(&self, reference: &DashboardRef, cx: &Context<Self>) -> Menu {
        let state = self.state.read(cx);
        let groups = state.groups();
        let Some((group, dashboard)) = state.dashboard(reference) else {
            return Menu::new("dashboard-menu").on_dismiss(Self::dismiss_listener(cx));
        };
        let index = group
            .dashboards
            .iter()
            .position(|candidate| candidate.id == dashboard.id)
            .unwrap_or(0);
        let last = group.dashboards.len().saturating_sub(1);
        let rename = RenameTarget::Dashboard(reference.clone());
        let mut menu = Menu::new("dashboard-menu")
            .item(Self::emit_item(
                "dashboard-edit",
                "edit dashboard…",
                SidebarEvent::EditDashboard(reference.clone()),
                cx,
            ))
            .item(
                MenuItem::new("dashboard-rename", "rename").on_click(cx.listener(
                    move |this, _: &ClickEvent, window, cx| {
                        this.start_rename(rename.clone(), window, cx);
                    },
                )),
            )
            .item({
                let target = reference.clone();
                Self::state_item("dashboard-duplicate", "duplicate", cx, move |state| {
                    state.duplicate_dashboard(&target).is_some()
                })
            })
            .separator();
        let up = reference.clone();
        let down = reference.clone();
        menu = menu
            .item(
                Self::state_item("dashboard-move-up", "move up", cx, move |state| {
                    state.move_dashboard(&up, -1)
                })
                .disabled(index == 0),
            )
            .item(
                Self::state_item("dashboard-move-down", "move down", cx, move |state| {
                    state.move_dashboard(&down, 1)
                })
                .disabled(index >= last),
            );
        let others: Vec<&DashboardGroup> = groups
            .iter()
            .filter(|other| other.id != reference.group_id)
            .collect();
        if !others.is_empty() {
            menu = menu.label("move to");
            for other in others {
                let target = reference.clone();
                let group_id = other.id.clone();
                menu = menu.item(Self::state_item(
                    gpui::ElementId::Name(format!("dashboard-move-to-{}", other.id).into()),
                    other.name.clone(),
                    cx,
                    move |state| state.move_dashboard_to(&target, &group_id).is_some(),
                ));
            }
        }
        menu = menu.separator().label("notifications");
        for (key, label, setting) in SCOPE_SETTINGS {
            let target = reference.clone();
            let checked = dashboard.notifications == setting;
            menu = menu.item(
                Self::state_item(format!("dashboard-notify-{key}"), label, cx, move |state| {
                    state.set_dashboard_notifications(&target, setting.clone())
                })
                .checked(checked),
            );
        }
        menu = menu.item(
            Self::emit_item(
                "dashboard-notify-custom",
                "custom rule…",
                SidebarEvent::CustomRule(ScopeKey::Dashboard(
                    reference.group_id.clone(),
                    reference.dashboard_id.clone(),
                )),
                cx,
            )
            .checked(matches!(dashboard.notifications, ScopeSetting::Custom(_))),
        );
        menu.separator()
            .item(Self::emit_item(
                "dashboard-delete",
                "delete dashboard…",
                SidebarEvent::DeleteDashboard(reference.clone()),
                cx,
            ))
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// The footer's `+` menu.
    pub(super) fn footer_menu(&self, cx: &Context<Self>) -> Menu {
        let has_environment = self.state.read(cx).environment().is_some();
        let has_groups = !self.state.read(cx).groups().is_empty();
        Menu::new("footer-menu")
            .item(
                Self::emit_item(
                    "footer-new-dashboard",
                    "new dashboard",
                    SidebarEvent::NewDashboard(None),
                    cx,
                )
                .key_hint(new_key())
                .disabled(!has_environment),
            )
            .item(
                MenuItem::new("footer-new-group", "new group")
                    .disabled(!has_environment)
                    .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                        this.menus.close();
                        let created = this
                            .state
                            .update(cx, |state, _| state.create_group("", None));
                        if let Some(id) = created {
                            this.start_rename(RenameTarget::Group(id), window, cx);
                        }
                        cx.notify();
                    })),
            )
            .separator()
            .item(
                Self::emit_item(
                    "footer-import",
                    "import dashboards…",
                    SidebarEvent::ImportGroups,
                    cx,
                )
                .disabled(!has_environment),
            )
            .item(
                Self::emit_item(
                    "footer-export",
                    "export all dashboards…",
                    SidebarEvent::ExportGroups(Vec::new()),
                    cx,
                )
                .disabled(!has_groups),
            )
            .on_dismiss(Self::dismiss_listener(cx))
    }

    /// The connection details above the footer status (ENV-06): the
    /// environment, the endpoint and its version, the state, the last
    /// event, the API user and "Reload from Icinga"; then the environment
    /// switcher (ENV-01) with "add environment…" and "edit …".
    pub(super) fn details_menu(&self, now: Timestamp, cx: &Context<Self>) -> Menu {
        let theme = cx.theme();
        let colors = theme.colors;
        let state = self.state.read(cx);
        let connection = state.connection();
        let line = |key: &'static str, value: String| {
            div()
                .flex()
                .gap(px(10.))
                .text_size(theme.text.small)
                .child(
                    div()
                        .flex_none()
                        .w(px(84.))
                        .text_color(colors.text_faint)
                        .child(key),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(colors.text)
                        .child(value),
                )
        };
        let mut menu = Menu::new("connection-details").min_width(px(320.));
        match state.environment() {
            Some(environment) => {
                let title = if state.is_demo() {
                    format!("{} (demo)", environment.name)
                } else {
                    environment.name.clone()
                };
                menu = menu.element(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.text_strong)
                                .child(title),
                        )
                        .child(
                            div()
                                .truncate()
                                .text_size(theme.text.small)
                                .text_color(colors.text_muted)
                                .child(environment.url.clone()),
                        ),
                );
                let lines = detail_lines(state, now)
                    .into_iter()
                    .map(|(key, value)| line(key, value));
                menu = menu
                    .separator()
                    .element(div().flex().flex_col().gap(px(4.)).children(lines));
                let can_reload = !connection.is_starting();
                menu = menu.separator().item(
                    MenuItem::new("reload", "Reload from Icinga")
                        .disabled(!can_reload)
                        .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.menus.close();
                            this.state.update(cx, |state, cx| {
                                if state.refresh(Instant::now()) {
                                    cx.notify();
                                }
                            });
                            cx.notify();
                        })),
                );
            }
            None => {
                menu = menu.element(
                    div()
                        .text_color(colors.text_muted)
                        .child("No environment configured."),
                );
            }
        }
        Self::with_switcher(menu, state, cx).on_dismiss(Self::dismiss_listener(cx))
    }

    /// The environment switcher (ENV-01) under the details: every
    /// environment, the active one checked, then "add environment…" and
    /// "edit …".
    fn with_switcher(mut menu: Menu, state: &AppState, cx: &Context<Self>) -> Menu {
        let active = state.active_environment_id().map(str::to_owned);
        let environments = state.environments();
        if !environments.is_empty() {
            menu = menu.separator().label("environments");
        }
        for environment in environments {
            let id = environment.id.clone();
            let is_active = active.as_deref() == Some(id.as_str());
            menu = menu.item(
                MenuItem::new(
                    gpui::ElementId::Name(format!("environment-{id}").into()),
                    environment.name.clone(),
                )
                .checked(is_active)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    if !is_active {
                        cx.emit(SidebarEvent::SwitchEnvironment(id.clone()));
                    }
                    cx.notify();
                })),
            );
        }
        menu = menu.separator().item(Self::emit_item(
            "add-environment",
            "add environment…",
            SidebarEvent::AddEnvironment,
            cx,
        ));
        if let Some(environment) = state.environment() {
            let id = environment.id.clone();
            menu = menu.item(
                MenuItem::new("edit-environment", format!("edit {}…", environment.name)).on_click(
                    cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.menus.close();
                        cx.emit(SidebarEvent::EditEnvironment(id.clone()));
                        cx.notify();
                    }),
                ),
            );
        }
        menu
    }
}

/// The connection details' lines: the state, the endpoint and its
/// version, the last event and the API user (with how many of the
/// permissions the client asks for it lacks).
pub(super) fn detail_lines(state: &AppState, now: Timestamp) -> Vec<(&'static str, String)> {
    let connection = state.connection();
    let mut lines = vec![("status", connection.describe(now))];
    if !connection.endpoint.is_empty() {
        lines.push(("endpoint", connection.endpoint.clone()));
    }
    if let Some(version) = connection.version() {
        lines.push(("version", version.to_owned()));
    }
    lines.push((
        "last event",
        connection.last_event_at.map_or_else(
            || "none yet".to_owned(),
            |at| format!("{} ago", format_compact(at.elapsed_until(now))),
        ),
    ));
    if let Some(info) = state.permissions() {
        let missing = ic_core::missing_permissions(info).len();
        lines.push((
            "API user",
            if missing == 0 {
                info.user.clone()
            } else {
                format!("{} · {missing} permissions missing", info.user)
            },
        ));
    }
    lines
}

/// The notification centre button's tooltip: unread notifications and a
/// pause.
pub(super) fn notifications_tooltip(
    unread: usize,
    paused_until: Option<Timestamp>,
    now: Timestamp,
) -> String {
    let mut text = "Notifications".to_owned();
    if unread > 0 {
        let _ = write!(text, " · {unread} unread");
    }
    if let Some(until) = paused_until.filter(|until| *until > now) {
        let _ = write!(text, " · paused until {}", crate::format::clock(until, now));
    }
    text
}

/// The shortcut for `ToggleSidebar`, as the tooltip shows it.
pub(super) fn toggle_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘B"
    } else {
        "ctrl-b"
    }
}

/// The shortcut for a new dashboard, as menus show it.
pub(crate) fn new_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘N"
    } else {
        "ctrl-n"
    }
}

#[cfg(test)]
mod tests {
    use ic_core::ApiInfo;
    use ic_model::Timestamp;

    use super::*;

    fn at(seconds: f64) -> Timestamp {
        Timestamp::from_unix_seconds(1_790_000_000. + seconds)
    }

    #[test]
    fn the_details_list_the_connection() {
        let mut state = AppState::fixture(at(0.));
        let lines = detail_lines(&state, at(65.));
        let keys: Vec<_> = lines.iter().map(|(key, _)| *key).collect();
        assert_eq!(keys, ["status", "endpoint", "version", "last event"]);
        assert_eq!(lines[0].1, "connected for 1m");
        assert_eq!(lines[1].1, "master-01");
        assert_eq!(lines[3].1, "1m ago");
        state.set_permissions(Some(ApiInfo {
            user: "viewer".to_owned(),
            permissions: vec!["objects/query/*".to_owned()],
            version: "v2.15.6".to_owned(),
        }));
        let lines = detail_lines(&state, at(65.));
        let (key, user) = lines.last().unwrap();
        assert_eq!(*key, "API user");
        assert!(user.starts_with("viewer · "), "{user}");
        assert!(user.ends_with("permissions missing"), "{user}");
    }

    #[test]
    fn the_notification_tooltip_counts_and_says_paused() {
        assert_eq!(notifications_tooltip(0, None, at(0.)), "Notifications");
        assert_eq!(
            notifications_tooltip(3, None, at(0.)),
            "Notifications · 3 unread"
        );
        let paused = notifications_tooltip(0, Some(at(600.)), at(0.));
        assert!(
            paused.starts_with("Notifications · paused until "),
            "{paused}"
        );
        assert_eq!(
            notifications_tooltip(0, Some(at(-1.)), at(0.)),
            "Notifications",
            "a pause that ended isn't mentioned"
        );
    }

    #[test]
    fn scope_settings_cover_inherit_on_and_off() {
        let settings: Vec<_> = SCOPE_SETTINGS.iter().map(|(_, _, s)| s.clone()).collect();
        assert_eq!(
            settings,
            [ScopeSetting::Inherit, ScopeSetting::On, ScopeSetting::Off]
        );
    }
}
