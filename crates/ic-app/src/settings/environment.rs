//! An environment's own page in the settings (16b–16b3), drilled into from
//! *icinga › environments* (its row) and from the health page's
//! *settings*: its connection (the editor holds it), and its **trouble
//! alerts**: how they notify (*notify* or *persistent*; no live data is
//! always on), and which heartbeats prove that Icinga runs its checks,
//! found by a custom variable or listed by name, with what icygui watches
//! of each. A beat that disappeared stays a finding until *confirm
//! removal*. Trouble alerts always notify; only a pause holds them back.
//!
//! The variable and the list apply on Enter and when the keyboard leaves
//! the field, as the panel's other fields do; the engine finds the beats
//! again at once.

use std::rc::Rc;

use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Focusable as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, div, prelude::FluentBuilder as _,
};
use ic_config::{DEFAULT_HEARTBEAT_VARIABLE, HeartbeatMode, TroublePolicy};
use ic_core::heartbeat::{BeatState, Heartbeat, HeartbeatSetup, Heartbeats};
use ic_model::{ServiceKey, Timestamp, format_two_units};
use ic_ui_kit::input::{InputEvent, InputState};
use ic_ui_kit::{
    Button, GlyphButton, Link, Menu, MenuItem, Segmented, Select, TextField, Theme,
    Tooltip, px,
};

use super::keyboard::Region;
use super::rows::{self, Row, SUB_INDENT};
use super::{SettingsMenu, SettingsPage, SettingsPanel};
use crate::cluster::beats::{BeatTone, tone_of};
use crate::menu_state::down_position;

/// The heartbeat variable's field (content width).
const VARIABLE_FIELD: f32 = 260.;
/// A listed heartbeat's field.
const ENTRY_FIELD: f32 = 300.;
/// The policy's dropdown.
const POLICY_DROPDOWN: f32 = 160.;
/// The beats table's columns: the dot, *proves*, *every*, *last beat*.
const DOT_COLUMN: f32 = 18.;
const PROVES_COLUMN: f32 = 150.;
const EVERY_COLUMN: f32 = 110.;
const LAST_COLUMN: f32 = 90.;
/// A field's padding and border around its content.
const FIELD_FRAME: f32 = 22.;

/// The drilled-into environment and its fields.
pub(super) struct EnvironmentPage {
    /// The environment's id.
    pub(super) id: String,
    /// The custom variable that marks heartbeats.
    variable: Entity<InputState>,
    /// The listed heartbeats, one field each (a new one is empty until
    /// typed into).
    entries: Vec<Entity<InputState>>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPanel {
    /// Shows environment `id`'s page (from its row, or the health page's
    /// *settings*), ending a search.
    pub(crate) fn show_environment(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(environment) = self.state.read(cx).environment_by_id(id).cloned() else {
            return;
        };
        self.commit_trouble(cx);
        self.show_page(SettingsPage::Icinga, window, cx);
        let beats = &environment.trouble.heartbeats;
        let mut subscriptions = Vec::new();
        let variable = self.trouble_input(
            beats.variable.clone(),
            DEFAULT_HEARTBEAT_VARIABLE,
            &mut subscriptions,
            window,
            cx,
        );
        let entries = beats
            .list
            .iter()
            .map(|entry| self.trouble_input(entry.clone(), "host!service", &mut subscriptions, window, cx))
            .collect();
        self.drill = Some(EnvironmentPage {
            id: id.to_owned(),
            variable,
            entries,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Back from an environment's page to the environments.
    pub(super) fn leave_environment(&mut self, cx: &mut Context<Self>) {
        if self.drill.is_some() {
            self.commit_trouble(cx);
            self.drill = None;
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// The id of the environment whose page shows.
    pub(crate) fn drilled_environment(&self) -> Option<&str> {
        self.drill.as_ref().map(|page| page.id.as_str())
    }

    /// A field of the trouble alerts that applies on Enter and when it
    /// loses the keyboard.
    fn trouble_input(
        &self,
        value: String,
        placeholder: &'static str,
        subscriptions: &mut Vec<Subscription>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(placeholder)
                .default_value(value)
        });
        subscriptions.push(cx.subscribe_in(
            &input,
            window,
            |this: &mut Self, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                    this.commit_trouble(cx);
                }
            },
        ));
        input
    }

    /// Applies the variable and the list as the fields have them.
    pub(super) fn commit_trouble(&mut self, cx: &mut Context<Self>) {
        let Some(page) = &self.drill else {
            return;
        };
        let id = page.id.clone();
        let variable = page.variable.read(cx).value().trim().to_owned();
        let list: Vec<String> = page
            .entries
            .iter()
            .map(|entry| entry.read(cx).value().trim().to_owned())
            .filter(|entry| !entry.is_empty())
            .collect();
        self.state.update(cx, |state, cx| {
            if state.set_trouble(&id, |trouble| {
                trouble.heartbeats.variable = variable;
                trouble.heartbeats.list = list;
            }) {
                cx.notify();
            }
        });
    }

    /// Changes the environment's policy or mode at once.
    fn change_trouble(&self, cx: &mut Context<Self>, change: impl FnOnce(&mut ic_config::Trouble)) {
        let Some(page) = &self.drill else {
            return;
        };
        let id = page.id.clone();
        self.state.update(cx, |state, cx| {
            if state.set_trouble(&id, change) {
                cx.notify();
            }
        });
    }

    /// *+ add*: an empty field for another heartbeat, with the keyboard.
    fn add_beat_entry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut subscriptions = Vec::new();
        let input = self.trouble_input(String::new(), "host!service", &mut subscriptions, window, cx);
        input.update(cx, |input, cx| input.focus(window, cx));
        if let Some(page) = &mut self.drill {
            page.entries.push(input);
            page._subscriptions.extend(subscriptions);
        }
        cx.notify();
    }

    /// `×`: the listed heartbeat goes.
    fn remove_beat_entry(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(page) = &mut self.drill
            && index < page.entries.len()
        {
            page.entries.remove(index);
        }
        self.commit_trouble(cx);
        cx.notify();
    }

    /// The environment's page: its connection and its trouble alerts.
    pub(super) fn render_environment(&self, theme: &Theme, cx: &Context<Self>) -> Vec<AnyElement> {
        let Some(page) = &self.drill else {
            return Vec::new();
        };
        let state = self.state.read(cx);
        let Some(environment) = state.environment_by_id(&page.id) else {
            return Vec::new();
        };
        let now = Timestamp::now();
        let heartbeats = state
            .snapshot_of(&page.id)
            .map(|snapshot| snapshot.heartbeats.as_ref().clone())
            .unwrap_or_default();
        let trouble = &environment.trouble;
        let colors = theme.colors;
        let id = page.id.clone();
        let urls = environment
            .urls
            .iter()
            .map(|url| url.url.trim_start_matches("https://").trim_end_matches('/').to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        let connection = div()
            .flex()
            .flex_col()
            .child(rows::section_label("connection", None, false, "", theme))
            .child(
                Row::new("URLs")
                    .control(
                        div()
                            .id("settings-environment-urls")
                            .max_w(px(420.))
                            .truncate()
                            .text_size(theme.text.small)
                            .text_color(colors.text_muted)
                            .cursor_pointer()
                            .hover(|style| style.text_color(colors.text))
                            .child(urls)
                            .tooltip(Tooltip::text("Edit the connection"))
                            .on_click(cx.listener(move |_, _: &ClickEvent, _, cx| {
                                cx.emit(super::SettingsEvent::EditEnvironment(id.clone()));
                            })),
                    )
                    .render(theme),
            );
        let mut alerts = div()
            .flex()
            .flex_col()
            .child(rows::section_label("trouble alerts", None, false, "", theme))
            .child(
                Row::new("policy")
                    .control(self.policy_dropdown(trouble.policy, theme, cx))
                    .render(theme),
            )
            .child(
                Row::new("no live data")
                    .control(
                        div()
                            .text_size(theme.text.small)
                            .text_color(colors.text_faint)
                            .child("always on"),
                    )
                    .render(theme),
            )
            .child(
                Row::new("heartbeats")
                    .control(self.mode_control(trouble.heartbeats.mode, theme, cx))
                    .render(theme),
            );
        alerts = match trouble.heartbeats.mode {
            HeartbeatMode::Find => {
                alerts.child(self.variable_row(page, &heartbeats, theme, cx))
                    .children(beats_table(&heartbeats, now, &page.id, theme, cx))
            }
            HeartbeatMode::List => alerts.children(self.list_rows(page, &heartbeats, theme, cx)),
        };
        alerts = alerts.child(
            div()
                .py(px(12.))
                .border_t_1()
                .border_color(colors.border_row)
                .text_size(theme.text.small)
                .text_color(colors.text_muted)
                .child("Trouble alerts always notify; only a pause holds them back."),
        );
        vec![connection.into_any_element(), alerts.into_any_element()]
    }

    /// *policy*: notify, or persistent (stays until dismissed).
    fn policy_dropdown(&self, policy: TroublePolicy, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let open = self.menus.is_open(&SettingsMenu::TroublePolicy);
        let mut menu = Menu::new("settings-trouble-policy-menu");
        for choice in TroublePolicy::ALL {
            menu = menu.item(
                MenuItem::new(
                    ElementId::Name(format!("settings-trouble-policy-{}", choice.label()).into()),
                    choice.label(),
                )
                .checked(policy == choice)
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.menus.close();
                    this.change_trouble(cx, |trouble| trouble.policy = choice);
                })),
            );
        }
        let menu = menu.on_dismiss(cx.listener(|this, dismissal: &ic_ui_kit::Dismissal, _, cx| {
            this.menus.dismissed(*dismissal);
            cx.notify();
        }));
        let trigger = div().flex_none().w(px(POLICY_DROPDOWN + FIELD_FRAME)).child(
            Select::new("settings-trouble-policy", policy.label())
                .open(open)
                .when(open, |select| select.menu(menu))
                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    this.menus
                        .toggle(SettingsMenu::TroublePolicy, down_position(event));
                    cx.notify();
                })),
        );
        self.focusable(
            "settings-trouble-policy",
            trigger,
            Rc::new(|this, _, cx| {
                this.menus.toggle(SettingsMenu::TroublePolicy, None);
                cx.notify();
            }),
            Some(Rc::new(move |this, forward, _, cx| {
                let next = if forward {
                    TroublePolicy::Persistent
                } else {
                    TroublePolicy::Notify
                };
                this.change_trouble(cx, |trouble| trouble.policy = next);
            })),
            theme,
            cx,
        )
    }

    /// *heartbeats*: find by custom variable, or list.
    fn mode_control(&self, mode: HeartbeatMode, theme: &Theme, cx: &Context<Self>) -> AnyElement {
        let selected = usize::from(mode == HeartbeatMode::List);
        let choose: Rc<dyn Fn(&mut Self, usize, &mut Context<Self>)> = Rc::new(|this, index, cx| {
            this.commit_trouble(cx);
            let mode = if index == 1 {
                HeartbeatMode::List
            } else {
                HeartbeatMode::Find
            };
            this.change_trouble(cx, |trouble| trouble.heartbeats.mode = mode);
        });
        let click = choose.clone();
        let step = choose.clone();
        let control = Segmented::new("settings-heartbeat-mode")
            .hug()
            .option("find by custom variable")
            .option("list")
            .selected(selected)
            .on_select(cx.listener(move |this, index: &usize, _, cx| click(this, *index, cx)));
        self.focusable(
            "settings-heartbeat-mode",
            control,
            Rc::new(move |this, _, cx| choose(this, 1 - selected, cx)),
            Some(Rc::new(move |this, forward, _, cx| {
                step(this, usize::from(forward), cx);
            })),
            theme,
            cx,
        )
    }

    /// *variable*, with what it finds: `found 6`, `found 5 · 1
    /// disappeared`, `none found`.
    fn variable_row(
        &self,
        page: &EnvironmentPage,
        heartbeats: &Heartbeats,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> AnyElement {
        let found = heartbeats
            .beats
            .iter()
            .filter(|beat| !matches!(beat.state, BeatState::Disappeared | BeatState::NotFound))
            .count();
        let gone = heartbeats
            .beats
            .iter()
            .filter(|beat| beat.state == BeatState::Disappeared)
            .count();
        let searching = matches!(heartbeats.setup, HeartbeatSetup::Find { .. });
        let (status, color) = if !searching {
            (String::new(), theme.colors.text_faint)
        } else if gone > 0 {
            (format!("found {found} · {gone} disappeared"), theme.states.text.warning)
        } else if found == 0 {
            ("none found".to_owned(), theme.colors.text_faint)
        } else {
            (format!("found {found}"), theme.states.text.ok)
        };
        let key = SharedString::from("settings-heartbeat-variable");
        self.add_stop(key.clone(), page.variable.focus_handle(cx), Region::Page);
        Row::new("variable")
            .indent(SUB_INDENT)
            .control(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .w(px(VARIABLE_FIELD + FIELD_FRAME))
                            .child(
                                TextField::new(&page.variable)
                                    .bordered(true)
                                    .text_size(theme.text.body),
                            )
                            .child(self.bounds_probe(key)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(170.))
                            .text_size(theme.text.small)
                            .text_color(color)
                            .child(status),
                    ),
            )
            .render(theme)
    }

    /// The list mode's rows: a field per heartbeat with what it proves or
    /// `not found`, `×`, and `+ add`.
    fn list_rows(
        &self,
        page: &EnvironmentPage,
        heartbeats: &Heartbeats,
        theme: &Theme,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = theme.colors;
        let mut rows: Vec<AnyElement> = page
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let text = entry.read(cx).value().trim().to_owned();
                let (status, bad) = entry_status(&text, heartbeats);
                let key = SharedString::from(format!("settings-heartbeat-entry-{index}"));
                self.add_stop(key.clone(), entry.focus_handle(cx), Region::Page);
                div()
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .py(px(8.))
                    .pl(px(SUB_INDENT))
                    .border_t_1()
                    .border_color(colors.border_row)
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .w(px(ENTRY_FIELD + FIELD_FRAME))
                            .child(
                                TextField::new(entry)
                                    .bordered(true)
                                    .invalid(bad)
                                    .text_size(theme.text.body),
                            )
                            .child(self.bounds_probe(key)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.text.small)
                            .text_color(if bad {
                                theme.states.text.critical
                            } else {
                                colors.text_muted
                            })
                            .child(status),
                    )
                    .child(
                        GlyphButton::new(
                            ElementId::Name(format!("settings-heartbeat-remove-{index}").into()),
                            "×",
                        )
                        .color(colors.text_muted)
                        .tooltip(Tooltip::new("Remove from the list"))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                            this.remove_beat_entry(index, cx);
                        })),
                    )
                    .into_any_element()
            })
            .collect();
        rows.push(
            div()
                .flex()
                .py(px(10.))
                .pl(px(SUB_INDENT))
                .border_t_1()
                .border_color(colors.border_row)
                .child(
                    Link::new("settings-heartbeat-add", "+ add").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.add_beat_entry(window, cx),
                    )),
                )
                .into_any_element(),
        );
        rows
    }
}

/// What a listed heartbeat's row says beside its field: `master-01 ·
/// every 30s`, `not found`, or how to write it; whether that is a
/// problem.
fn entry_status(text: &str, heartbeats: &Heartbeats) -> (String, bool) {
    if text.is_empty() {
        return (String::new(), false);
    }
    let Some(key) = ServiceKey::parse(text) else {
        return ("write it as host!service".to_owned(), true);
    };
    match heartbeats.beats.iter().find(|beat| beat.key == key) {
        Some(beat) if beat.state == BeatState::NotFound => ("not found".to_owned(), true),
        Some(beat) => (
            format!("{} · every {}", beat.proves.label(), interval_text(beat)),
            false,
        ),
        None => (String::new(), false),
    }
}

/// A beat's interval (`30s`, `5m`).
fn interval_text(beat: &Heartbeat) -> String {
    format_two_units(beat.interval)
}

/// The table of the heartbeats found (16b, 16b3): what each proves, its
/// object, its interval and the age of its last beat; a disappeared one
/// says since when, with *confirm removal*.
fn beats_table(
    heartbeats: &Heartbeats,
    now: Timestamp,
    environment: &str,
    theme: &Theme,
    cx: &Context<SettingsPanel>,
) -> Vec<AnyElement> {
    if heartbeats.beats.is_empty() {
        return Vec::new();
    }
    let colors = theme.colors;
    let header = div()
        .flex()
        .items_center()
        .gap(px(12.))
        .h(px(32.))
        .pl(px(SUB_INDENT))
        .text_size(theme.text.small)
        .text_color(colors.text_faint)
        .child(div().flex_none().w(px(DOT_COLUMN)))
        .child(div().flex_none().w(px(PROVES_COLUMN)).child("proves"))
        .child(div().flex_1().min_w_0().child("object"))
        .child(div().flex_none().w(px(EVERY_COLUMN)).child("every"))
        .child(
            div()
                .flex()
                .flex_none()
                .w(px(LAST_COLUMN))
                .justify_end()
                .child("last beat"),
        );
    let mut rows = vec![header.into_any_element()];
    for beat in &heartbeats.beats {
        let tone = tone_of(beat);
        let gone = beat.state == BeatState::Disappeared;
        let not_found = beat.state == BeatState::NotFound;
        let age = beat.last_beat.map_or_else(
            || "—".to_owned(),
            |at| format_two_units(at.elapsed_until(now)),
        );
        let row = div()
            .id(ElementId::Name(format!("settings-beat-{}", beat.object()).into()))
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(40.))
            .pl(px(SUB_INDENT))
            .border_t_1()
            .border_color(colors.border_row)
            .text_size(theme.text.row)
            .whitespace_nowrap()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(DOT_COLUMN))
                    .child(ic_ui_kit::StateDot::with_color(tone.fill(theme)).size(px(7.))),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(PROVES_COLUMN))
                    .truncate()
                    .text_color(colors.text_strong)
                    .child(beat.proves.label()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.text_faint)
                    .child(beat.object()),
            );
        let row = if gone {
            let key = beat.key.clone();
            let environment = environment.to_owned();
            row.child(
                div()
                    .flex_none()
                    .text_size(theme.text.small)
                    .text_color(theme.states.text.warning)
                    .child(format!(
                        "disappeared since {}",
                        crate::format::list_clock(beat.since, now)
                    )),
            )
            .child(
                Button::new(
                    ElementId::Name(format!("settings-beat-confirm-{}", beat.object()).into()),
                    "confirm removal",
                )
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    let key = key.clone();
                    this.state.update(cx, |state, cx| {
                        state.confirm_heartbeat_removal(&environment, key);
                        cx.notify();
                    });
                })),
            )
        } else {
            row.child(
                div()
                    .flex_none()
                    .w(px(EVERY_COLUMN))
                    .text_color(colors.text_muted)
                    .child(interval_text(beat)),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .w(px(LAST_COLUMN))
                    .justify_end()
                    .text_color(match tone {
                        BeatTone::Critical => theme.states.text.critical,
                        BeatTone::Warning => theme.states.text.warning,
                        BeatTone::Ok | BeatTone::Off => colors.text,
                    })
                    .child(if not_found { "not found".to_owned() } else { age }),
            )
        };
        rows.push(row.into_any_element());
    }
    rows
}

impl SettingsPanel {
    /// What the header says over an environment's page.
    pub(super) fn environment_title(&self, cx: &App) -> Option<String> {
        let page = self.drill.as_ref()?;
        self.state
            .read(cx)
            .environment_by_id(&page.id)
            .map(|environment| environment.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use ic_core::heartbeat::Proves;

    use super::*;

    fn beat(name: &str, state: BeatState) -> Heartbeat {
        Heartbeat {
            key: ServiceKey::new("icygui-hb-master", name),
            proves: Proves::Endpoint {
                endpoint: "master-01".to_owned(),
                zone: Some("master".to_owned()),
            },
            interval: Duration::from_secs(30),
            state,
            last_beat: None,
            last_check: None,
            since: Timestamp::from_unix_seconds(1_790_000_000.),
            deadline: None,
            allowance: Duration::from_secs(5),
            reason: None,
        }
    }

    #[test]
    fn listed_heartbeats_say_what_they_prove_or_why_not() {
        let heartbeats = Heartbeats {
            setup: HeartbeatSetup::List,
            beats: vec![
                beat("beat-master-01", BeatState::OnTime),
                beat("beet", BeatState::NotFound),
            ],
            polled: false,
        };
        assert_eq!(
            entry_status("icygui-hb-master!beat-master-01", &heartbeats),
            ("master-01 · every 30s".to_owned(), false)
        );
        assert_eq!(
            entry_status("icygui-hb-master!beet", &heartbeats),
            ("not found".to_owned(), true)
        );
        assert_eq!(
            entry_status("no-separator", &heartbeats),
            ("write it as host!service".to_owned(), true)
        );
        assert_eq!(entry_status("", &heartbeats), (String::new(), false));
        assert_eq!(
            entry_status("other!beat", &heartbeats),
            (String::new(), false),
            "not looked for yet"
        );
    }
}
