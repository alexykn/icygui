//! The service pane's body (screen 2b): state and name, actions, plugin
//! output, performance data, check details, comments and downtimes, custom
//! variables, groups, notes and links.

use gpui::{
    AnyElement, ClickEvent, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Styled as _, div, prelude::FluentBuilder as _, px,
};
use ic_core::snapshot::Snapshot;
use ic_model::{CheckableState, CommentKind, Links, ObjectKey, Service, ServiceState, Timestamp};
use ic_ui_kit::{
    ActiveTheme as _, CircleSize, CodeBlock, IconButton, IconName, KvTable, Link, NoteEntry,
    PerfdataTable, SectionLabel, StateCircle, Theme, Tooltip, TreeTable,
};

use super::{
    BodyLayout, ObjectPane, TAB_COLUMN_GAP, TAB_CONTENT_WIDTH, TAB_SIDE_WIDTH, TITLE_GROUP,
    action_buttons, copy_button, model, scroll_area, web_link,
};
use crate::actions::ObjectAction;

/// The hover group of the plugin output (reveals its copy button).
const OUTPUT_GROUP: &str = "pane-output";

/// The hover group of a comment or downtime (reveals its remove button).
const NOTE_GROUP: &str = "pane-note";

/// Space between the body's sections.
const SECTION_GAP: f32 = 24.;

pub(super) fn render(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    service: &Service,
    now: Timestamp,
    layout: BodyLayout,
    cx: &Context<ObjectPane>,
) -> AnyElement {
    let theme = cx.theme();
    let key = service.object_key();
    let host = snapshot.host_of(&service.key);
    let host_problem = host.is_some_and(|host| host.is_problem());
    let handled = service.is_handled(host_problem);
    let acknowledged = service.check.acknowledgement.is_acknowledged();

    let output = output(service, theme);
    let perfdata = service
        .check
        .result
        .as_ref()
        .filter(|result| !result.perfdata.is_empty())
        .map(|result| {
            // Beside the list the pane has room for the design's three
            // columns; a tab also shows min and max.
            PerfdataTable::new(&result.perfdata)
                .show_range(layout != BodyLayout::Pane)
                .into_any_element()
        });
    let readable = pane.state.read(cx).can_read_notifications();
    let check = check_table(snapshot, &key, service, readable, now).into_any_element();
    let notes = notes(pane, snapshot, &key, now, cx);
    let vars = vars(service).map(IntoElement::into_any_element);
    let groups =
        groups(snapshot, service, host.map(|host| host.groups.as_slice())).into_any_element();
    let links = links_table(
        &service.links,
        model::MacroScope {
            host: host.map(AsRef::as_ref),
            service: Some(service),
        },
    )
    .map(IntoElement::into_any_element);

    let column = sections()
        .px(theme.metrics.pane_inset)
        .py(theme.metrics.pane_padding)
        .child(title(
            service,
            host.map(|host| host.display_name.as_str()),
            handled,
            crate::dashboard::rows::late_label(snapshot, &key, now),
            now,
            cx,
        ))
        .child(action_buttons(pane, acknowledged, service.is_problem(), cx));
    let column = match layout {
        BodyLayout::Pane | BodyLayout::Tab => column
            .when(layout == BodyLayout::Tab, |column| {
                column.max_w(px(TAB_CONTENT_WIDTH))
            })
            .child(output)
            .children(perfdata)
            .child(check)
            .children(notes)
            .children(vars)
            .child(groups)
            .children(links),
        BodyLayout::WideTab => column
            .max_w(px(TAB_CONTENT_WIDTH + TAB_COLUMN_GAP + TAB_SIDE_WIDTH))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(TAB_COLUMN_GAP))
                    .child(
                        sections()
                            .flex_1()
                            .min_w_0()
                            .child(output)
                            .children(perfdata)
                            .children(notes),
                    )
                    .child(
                        sections()
                            .flex_none()
                            .w(px(TAB_SIDE_WIDTH))
                            .child(check)
                            .children(vars)
                            .child(groups)
                            .children(links),
                    ),
            ),
    };
    scroll_area("service-pane-body", &pane.scroll, column).into_any_element()
}

/// A column of sections.
fn sections() -> gpui::Div {
    div().flex().flex_col().gap(px(SECTION_GAP))
}

fn title(
    service: &Service,
    host_name: Option<&str>,
    handled: bool,
    late: Option<String>,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> impl IntoElement {
    let theme = cx.theme();
    let colors = theme.colors;
    let host_key = ObjectKey::Host {
        name: service.key.host.clone(),
    };
    let host_label = host_name.map_or_else(|| service.key.host.to_string(), str::to_owned);
    div()
        .group(TITLE_GROUP)
        .flex()
        .items_center()
        .gap(px(16.))
        .child(
            StateCircle::new(CheckableState::Service(service.state))
                .size(CircleSize::Pane)
                .handled(handled)
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
                                .child(service.display_name.clone()),
                        )
                        .child(copy_button(
                            "copy-name",
                            service.key.full_name(),
                            "Copy name",
                            TITLE_GROUP,
                            theme,
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .text_size(theme.text.body)
                        .text_color(colors.text_muted)
                        .child("on\u{a0}")
                        .child(
                            Link::new("host-link", host_label)
                                .tooltip(Tooltip::new("Show the host"))
                                .on_click(cx.listener(
                                    move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                                        pane.navigate(host_key.clone(), cx);
                                    },
                                )),
                        )
                        .child(format!("\u{a0}· {}", model::service_subtitle(service, now)))
                        .when_some(late, |line, late| {
                            line.child(
                                div()
                                    .text_color(theme.states.warning)
                                    .child(format!("\u{a0}· {late}")),
                            )
                        }),
                ),
        )
}

fn output(service: &Service, theme: &Theme) -> AnyElement {
    let Some(result) = &service.check.result else {
        let text = if service.state == ServiceState::Pending {
            "waiting for the first check result"
        } else {
            "the output isn't loaded yet"
        };
        return div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .child(SectionLabel::new("plugin output"))
            .child(
                div()
                    .text_size(theme.text.body)
                    .text_color(theme.colors.text_muted)
                    .child(text),
            )
            .into_any_element();
    };
    let text = if result.long_output.is_empty() {
        result.output.clone()
    } else {
        format!("{}\n{}", result.output, result.long_output)
    };
    div()
        .group(OUTPUT_GROUP)
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .h(px(16.))
                .child(SectionLabel::new("plugin output"))
                .child(copy_button(
                    "copy-output",
                    text.clone(),
                    "Copy output",
                    OUTPUT_GROUP,
                    theme,
                )),
        )
        .child(CodeBlock::new(text))
        .into_any_element()
}

fn check_table(
    snapshot: &Snapshot,
    key: &ObjectKey,
    service: &Service,
    readable: Option<bool>,
    now: Timestamp,
) -> KvTable {
    model::object_check_rows(snapshot, key, &service.check, readable, now)
        .into_iter()
        .fold(KvTable::new().title("check"), |table, (key, value)| {
            table.row(key, value)
        })
}

/// Comments, acknowledgements and downtimes, each with a remove button
/// (acknowledgements are removed with the "remove ack" button).
pub(super) fn notes(
    pane: &ObjectPane,
    snapshot: &Snapshot,
    key: &ObjectKey,
    now: Timestamp,
    cx: &Context<ObjectPane>,
) -> Option<AnyElement> {
    let theme = cx.theme();
    let comments = snapshot
        .comments
        .get(key)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let downtimes = snapshot
        .downtimes
        .get(key)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if comments.is_empty() && downtimes.is_empty() {
        return None;
    }
    let state = pane.state.read(cx);
    // Shown while the mouse is over its note, like the copy buttons;
    // disabled with the reason when the API user may not remove it.
    let remove = |id: String, tooltip: &'static str, action: ObjectAction| {
        let button = IconButton::new(gpui::SharedString::from(id), IconName::Close)
            .size(px(20.))
            .icon_size(px(12.))
            .color(theme.colors.text_faint);
        let button = match state.action_denial(&action) {
            Some(denial) => button.disabled(true).tooltip(Tooltip::new(denial)),
            None => button.tooltip(Tooltip::new(tooltip)).on_click(cx.listener(
                move |pane: &mut ObjectPane, _: &ClickEvent, _, cx| {
                    pane.request(action.clone(), cx);
                },
            )),
        };
        div()
            .flex_none()
            .invisible()
            .group_hover(NOTE_GROUP, gpui::Styled::visible)
            .child(button)
    };
    let mut column = div().flex().flex_col().gap(px(14.));
    for comment in comments {
        let note = model::comment_note(comment, now);
        let mut entry = note_entry(&note);
        if comment.kind != CommentKind::Acknowledgement {
            entry = entry.child(remove(
                format!("remove-comment-{}", note.name),
                "Remove comment",
                ObjectAction::RemoveComment(note.name.clone()),
            ));
        }
        column = column.child(div().group(NOTE_GROUP).child(entry));
    }
    for downtime in downtimes {
        let note = model::downtime_note(downtime, now);
        let entry = note_entry(&note).child(remove(
            format!("remove-downtime-{}", note.name),
            "Remove downtime",
            ObjectAction::RemoveDowntime(note.name.clone()),
        ));
        column = column.child(div().group(NOTE_GROUP).child(entry));
    }
    Some(column.into_any_element())
}

fn note_entry(note: &model::Note) -> NoteEntry {
    note.meta.iter().fold(
        NoteEntry::new(note.author.clone(), note.body.clone()).marker(note.marker),
        |entry, meta| entry.meta(meta.clone()),
    )
}

fn vars(service: &Service) -> Option<TreeTable> {
    let lines = model::vars_lines(&service.vars);
    (!lines.is_empty()).then(|| {
        TreeTable::new(lines)
            .title("custom vars")
            .key_width(px(140.))
    })
}

fn groups(snapshot: &Snapshot, service: &Service, host_groups: Option<&[String]>) -> KvTable {
    let service_groups = model::group_names(&service.groups, |name| {
        snapshot
            .service_groups
            .iter()
            .find(|group| group.name == name)
            .map(|group| group.display_name.clone())
    });
    let host_groups = model::group_names(host_groups.unwrap_or_default(), |name| {
        snapshot
            .host_groups
            .iter()
            .find(|group| group.name == name)
            .map(|group| group.display_name.clone())
    });
    let none = |names: String| {
        if names.is_empty() {
            "—".to_owned()
        } else {
            names
        }
    };
    KvTable::new()
        .title("groups")
        .row("service", none(service_groups))
        .row("host", none(host_groups))
}

/// Notes and links, if the object has any. The URLs' macros are resolved
/// against `scope`, and an attribute listing several URLs shows each.
pub(super) fn links_table(links: &Links, scope: model::MacroScope<'_>) -> Option<KvTable> {
    let notes = links.notes.trim();
    let urls = [
        ("notes url", model::link_urls(&links.notes_url, scope)),
        ("action url", model::link_urls(&links.action_url, scope)),
    ];
    if notes.is_empty() && urls.iter().all(|(_, urls)| urls.is_empty()) {
        return None;
    }
    let mut table = KvTable::new().title("notes");
    if !notes.is_empty() {
        table = table.row("notes", notes.to_owned());
    }
    for (label, urls) in urls {
        if urls.is_empty() {
            continue;
        }
        let column = urls.iter().enumerate().fold(
            div().flex().flex_col().gap(px(4.)).min_w_0(),
            |column, (index, url)| {
                if model::is_web_link(url) {
                    column.child(web_link(format!("link-{label}-{index}").into(), url))
                } else {
                    // A relative URL needs Icinga Web's address, which the
                    // client doesn't know.
                    column.child(div().truncate().child(url.clone()))
                }
            },
        );
        table = table.row(label, column);
    }
    Some(table)
}
