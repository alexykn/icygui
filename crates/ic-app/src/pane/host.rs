//! The host pane's body (screen 2c): state, address and uptime, actions,
//! and the sub-tabs `services · history · vars · config`.
//!
//! The title, the actions and the sub-tab strip stay put; everything under
//! them scrolls, including the host's comments and downtimes (at the top of
//! the services tab), so a host with many notes can't push its services out
//! of reach.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, div,
    prelude::FluentBuilder as _,
};
use ic_config::ListTimes;
use ic_core::snapshot::Snapshot;
use ic_model::{Host, ObjectKey, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, CircleSize, CompactRow, KvTable, Link, ObjectMark, StateCircle, SubTabs,
    Theme, Tooltip, TreeTable, px,
};

use super::service::{full_output, links_table, notes};
use super::{
    HostTab, ObjectPane, PaneMode, TAB_CONTENT_WIDTH, TITLE_GROUP, action_buttons, copy_button,
    model, scroll_area,
};
use crate::format;

pub(super) fn render(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    host: &Host,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let theme = cx.theme();
    let key = host.key();
    let services = model::host_services(snapshot, host, pane.show_all_ok);
    let entity = cx.entity();
    let tabs = SubTabs::new("host-tabs")
        .tab_with_count("services", services.total)
        .tab("history")
        .tab("vars")
        .tab("config")
        .selected(pane.host_tab.index())
        .on_select(move |index, _, cx| {
            let tab = HostTab::ALL.get(index).copied().unwrap_or_default();
            entity.update(cx, |pane, cx| pane.select_host_tab(tab, cx));
        });
    let top = div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(20.))
        .px(theme.metrics.pane_inset)
        .pt(theme.metrics.pane_padding)
        .child(title(
            host,
            crate::dashboard::rows::late_label(snapshot, &key, now),
            now,
            theme,
        ))
        .child(action_buttons(
            pane,
            host.check.acknowledgement.is_acknowledged(),
            host.is_problem(),
            full_output(&host.check),
            cx,
        ))
        .child(tabs);
    let content = match pane.host_tab {
        HostTab::Services => {
            // Comments, then the downtimes the banner doesn't show, above
            // the services (topic 01).
            let block = |content: AnyElement| {
                div()
                    .px(theme.metrics.pane_inset)
                    .py(px(16.))
                    .border_b_1()
                    .border_color(theme.colors.border_row)
                    .child(content)
            };
            let notes = notes(pane, snapshot, &key, now, cx)
                .into_iter()
                .chain(super::downtime::others(pane, snapshot, &key, now, cx))
                .map(block);
            div()
                .flex()
                .flex_col()
                .children(notes)
                .child(services_tab(pane, &services, host, now, cx))
                .into_any_element()
        }
        HostTab::History => super::history::host_tab(pane, &host.display_name, now, cx),
        HostTab::Vars => vars_tab(host, theme),
        HostTab::Config => config_tab(pane, snapshot, host, now, cx),
    };
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .when(pane.mode == PaneMode::Tab, |body| {
            body.max_w(px(TAB_CONTENT_WIDTH))
        })
        .child(top)
        .child(scroll_area("host-pane-body", &pane.scroll, content))
        .into_any_element()
}

fn title(host: &Host, late: Option<String>, now: Timestamp, theme: &Theme) -> impl IntoElement {
    let colors = theme.colors;
    div()
        .group(TITLE_GROUP)
        .flex()
        .items_center()
        .gap(px(16.))
        .child(
            StateCircle::mark(ObjectMark::host(host))
                .size(CircleSize::Pane)
                .state_label(),
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
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_size(theme.text.title)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors.text_strong)
                                .child(host.display_name.clone()),
                        )
                        .child(copy_button(
                            "copy-name",
                            host.name.to_string(),
                            "Copy name",
                            TITLE_GROUP,
                            theme,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .min_w_0()
                        .text_size(theme.text.body)
                        .text_color(colors.text_muted)
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .child(model::host_subtitle(host, now)),
                        )
                        .when_some(late, |line, late| {
                            line.child(
                                div()
                                    .flex_none()
                                    .text_color(theme.states.text.warning)
                                    .child(format!("\u{a0}· {late}")),
                            )
                        }),
                ),
        )
}

fn services_tab(
    pane: &ObjectPane,
    services: &model::HostServices,
    host: &Host,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let theme = cx.theme();
    let times = pane.state.read(cx).appearance().list_times;
    let rows = services.shown.iter().map(|service| {
        let key = service.object_key();
        CompactRow::new(SharedString::from(format!(
            "host-service-{}",
            service.key.name
        )))
        .leading(
            StateCircle::mark(ObjectMark::service(service, Some(host))).size(CircleSize::Compact),
        )
        .title(service.display_name.clone())
        .detail(service.check.output().to_owned())
        .trailing(match times {
            ListTimes::Relative => format::time_in_state(&service.check, now),
            ListTimes::Clock => format::state_clock(&service.check, now),
        })
        .on_click(
            cx.listener(move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                pane.navigate(key.clone(), cx);
            }),
        )
    });
    // `+ N more`, and `− show fewer` in the same slot (the shared paging
    // rule, as in every host-with-services view).
    let toggle = services
        .pages
        .then(|| crate::paging::more_label(services.hidden));
    div()
        .flex()
        .flex_col()
        .children(rows)
        .when_some(toggle, |column, label| {
            column.child(
                div()
                    .id("more-ok")
                    .px(theme.metrics.pane_inset)
                    .pt(px(10.))
                    .pb(px(12.))
                    .text_size(theme.text.small)
                    .text_color(theme.colors.text_faint)
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme.colors.text))
                    .child(label)
                    .on_click(cx.listener(|pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                        pane.show_all_ok = !pane.show_all_ok;
                        cx.notify();
                    })),
            )
        })
        .into_any_element()
}

fn vars_tab(host: &Host, theme: &Theme) -> AnyElement {
    let lines = model::vars_lines(&host.vars);
    let body = div().px(theme.metrics.pane_inset).py(px(16.));
    if lines.is_empty() {
        return body
            .text_size(theme.text.small)
            .text_color(theme.colors.text_muted)
            .child("no custom variables")
            .into_any_element();
    }
    body.child(TreeTable::new(lines)).into_any_element()
}

fn config_tab(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    host: &Host,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let theme = cx.theme();
    let readable = pane.state.read(cx).can_read_notifications();
    let check = model::object_check_rows(snapshot, &host.key(), &host.check, readable, now)
        .into_iter()
        .fold(KvTable::new().title("check"), |table, (key, value)| {
            table.row(key, value)
        });
    let features = model::feature_rows(host.check.features).into_iter().fold(
        KvTable::new().title("features (read-only)"),
        |table, (name, enabled)| {
            table.row(
                name,
                div()
                    .text_color(if enabled {
                        theme.colors.text
                    } else {
                        theme.colors.text_faint
                    })
                    .child(if enabled { "on" } else { "off" }),
            )
        },
    );
    let groups = model::group_names(&host.groups, |name| {
        snapshot
            .host_groups
            .iter()
            .find(|group| group.name == name)
            .map(|group| group.display_name.clone())
    });
    let dash = |text: &str| {
        if text.is_empty() {
            "—".to_owned()
        } else {
            text.to_owned()
        }
    };
    let object = KvTable::new()
        .title("host")
        .row("address", dash(&host.address))
        .row("address6", dash(&host.address6))
        .row("groups", dash(&groups));
    let (parents, children) = model::host_relations(snapshot, host);
    let relations = (!parents.is_empty() || !children.is_empty()).then(|| {
        KvTable::new()
            .title("dependencies")
            .row("parents", host_links("parent", &parents, cx))
            .row("children", host_links("child", &children, cx))
    });
    div()
        .flex()
        .flex_col()
        .gap(px(24.))
        .px(theme.metrics.pane_inset)
        .py(px(16.))
        .child(check)
        .child(features)
        .child(object)
        .children(relations)
        .children(links_table(
            &host.links,
            model::MacroScope {
                host: Some(host),
                service: None,
            },
        ))
        .into_any_element()
}

/// Links to hosts that open them in the pane.
fn host_links(prefix: &str, hosts: &[ObjectKey], cx: &Context<ObjectPane>) -> AnyElement {
    if hosts.is_empty() {
        return div().child("—").into_any_element();
    }
    div()
        .flex()
        .flex_wrap()
        .gap_x(px(12.))
        .children(hosts.iter().map(|key| {
            let target = key.clone();
            Link::new(
                SharedString::from(format!("{prefix}-{key}")),
                key.full_name(),
            )
            .tooltip(Tooltip::new("Show the host"))
            .on_click(cx.listener(
                move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                    pane.navigate(target.clone(), cx);
                },
            ))
        }))
        .into_any_element()
}
