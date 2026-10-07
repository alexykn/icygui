//! Operator actions in the window (ACT-01..07, LIST-03, PANE-05, ENV-09)
//! on the fixture with a recording core: the dialogs by keyboard (focus,
//! Tab, Enter, Escape, validation), bulk actions on marked rows, what is
//! skipped, confirmations, refusals and toasts. `live_actions` runs every
//! action through the real core against the demo's mock Icinga.

use gpui::{App, Entity, Modifiers};
use ic_core::{ActionOutcome, ApiInfo, CoreEvent};
use ic_model::{Action, ActionTarget, CommandType, DowntimeMode, ObjectKey};

use super::{Harness, replication, row_position, run, shift};
use crate::actions::ObjectAction;
use crate::app_state::NOT_CONNECTED;
use crate::app_state::testing::Recorder;
use crate::fixture::FixtureOptions;
use crate::operate::dialog::{ActionDialog, DialogKind, Form};
use crate::operate::forms::FormField;
use crate::operate::tracker::ToastTone;
use crate::palette::PaletteCommand;
use crate::pane::{ObjectPane, PaneMenu};
use crate::workspace::{Confirmed, ModalKind};

/// Attaches a recording core (the fixture is connected but has none).
pub(super) fn record(app: &Harness, cx: &mut App) -> Recorder {
    let recorder = Recorder::default();
    app.state
        .update(cx, |state, _| state.set_core(Box::new(recorder.clone())));
    recorder
}

fn modal(app: &Harness, cx: &App) -> Option<ModalKind> {
    app.workspace.read(cx).modal(cx)
}

fn dialog(app: &Harness, cx: &App) -> Entity<ActionDialog> {
    app.workspace
        .read(cx)
        .action_dialog()
        .cloned()
        .expect("an action dialog")
}

fn focused(app: &Harness, cx: &mut App) -> Option<FormField> {
    let dialog = dialog(app, cx);
    app.in_window(cx, |window, cx| dialog.read(cx).focused_field(window, cx))
}

fn type_into(app: &Harness, cx: &mut App, field: FormField, text: &str) {
    let dialog = dialog(app, cx);
    app.in_window(cx, |window, cx| {
        ActionDialog::type_into(&dialog, field, text, window, cx);
    });
    app.draw(cx);
}

/// Asks for `action` on `targets`, as a key or a button does.
pub(super) fn request(app: &Harness, cx: &mut App, action: ObjectAction, targets: Vec<ObjectKey>) {
    app.state.update(cx, |state, cx| {
        let _ = state.request(crate::actions::ActionRequest {
            action,
            targets,
            review: false,
        });
        cx.notify();
    });
    app.draw(cx);
}

pub(super) fn toasts(app: &Harness, cx: &App) -> Vec<(ToastTone, String, Vec<String>)> {
    app.state
        .read(cx)
        .toasts()
        .map(|toast| (toast.tone, toast.title.clone(), toast.lines.clone()))
        .collect()
}

/// An object of the fixture that `pick` likes.
fn find(app: &Harness, cx: &App, pick: impl Fn(&ic_model::Service) -> bool) -> ObjectKey {
    app.state
        .read(cx)
        .snapshot()
        .services
        .values()
        .find(|service| pick(service))
        .map(|service| service.object_key())
        .expect("the fixture has such a service")
}

#[test]
fn acknowledging_by_keyboard_sends_one_action_and_reports_it() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.keys(cx, "j j");
        assert_eq!(app.cursor(cx).map(|(_, key)| key), Some(replication()));
        app.keys(cx, "a");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::Acknowledge))
        );
        assert_eq!(
            focused(app, cx),
            Some(FormField::Comment),
            "typing goes to the comment"
        );
        // The list's keys don't fire while typing.
        app.keys(cx, "j k x");
        assert!(app.marked(cx).is_empty());

        // Enter without a comment: the problem shows, nothing is sent.
        type_into(app, cx, FormField::Comment, "");
        app.keys(cx, "enter");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::Acknowledge))
        );
        assert!(
            dialog(app, cx)
                .read(cx)
                .shown_issues()
                .contains_key(&FormField::Comment)
        );
        assert!(recorder.actions().is_empty());

        type_into(app, cx, FormField::Comment, "looking into it");
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None, "sent: the dialog closes");
        assert_eq!(
            recorder.actions(),
            [(
                1,
                ActionTarget::Objects(vec![replication()]),
                Action::Acknowledge {
                    comment: "looking into it".to_owned(),
                    sticky: false,
                    persistent: false,
                    expiry: None,
                }
            )]
        );
        // On its way: the row and the toast say so.
        assert_eq!(
            app.state.read(cx).pending_label(&replication()),
            Some("ack pending…")
        );
        assert_eq!(
            toasts(app, cx),
            [(
                ToastTone::Pending,
                "Acknowledging postgres-replication on db-prod-03…".to_owned(),
                Vec::new()
            )]
        );
        // The core answers.
        app.state.update(cx, |state, cx| {
            state.apply(CoreEvent::ActionFinished {
                id: 1,
                outcome: ActionOutcome {
                    ok: 1,
                    ..ActionOutcome::default()
                },
            });
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(
            toasts(app, cx)[0].1,
            "Acknowledged postgres-replication on db-prod-03"
        );
        assert_eq!(toasts(app, cx)[0].0, ToastTone::Success);
    });
}

#[test]
fn tab_moves_between_the_fields_shown_and_escape_cancels() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.keys(cx, "j j d");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::Downtime))
        );
        assert_eq!(focused(app, cx), Some(FormField::Comment));
        app.keys(cx, "tab");
        assert_eq!(focused(app, cx), Some(FormField::Start));
        app.keys(cx, "tab");
        assert_eq!(focused(app, cx), Some(FormField::End));
        // The duration only shows for flexible downtimes.
        app.keys(cx, "tab");
        assert_eq!(focused(app, cx), Some(FormField::Trigger));
        app.keys(cx, "tab");
        assert_eq!(focused(app, cx), Some(FormField::Comment), "around again");
        app.keys(cx, "shift-tab");
        assert_eq!(focused(app, cx), Some(FormField::Trigger));
        dialog(app, cx).update(cx, |dialog, cx| {
            dialog.edit_form(cx, |form| {
                if let Form::Downtime(form) = form {
                    form.flexible = true;
                }
            });
        });
        app.draw(cx);
        app.keys(cx, "shift-tab");
        assert_eq!(focused(app, cx), Some(FormField::Duration));

        app.keys(cx, "escape");
        assert_eq!(modal(app, cx), None);
        assert!(recorder.actions().is_empty(), "cancelled: nothing sent");
        // The list has the keyboard again.
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(1));
        app.keys(cx, "j");
        assert_eq!(app.cursor(cx).map(|(index, _)| index), Some(2));
    });
}

#[test]
fn downtimes_take_presets_and_check_their_window() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        request(
            app,
            cx,
            ObjectAction::ScheduleDowntime,
            vec![ObjectKey::host("db-prod-03")],
        );
        // All services, on by default: the box lists the host and each of
        // its services, and the button counts them (topic 01).
        let services = app
            .state
            .read(cx)
            .snapshot()
            .services_of(&ic_model::HostName::from("db-prod-03"))
            .count();
        let listed = dialog(app, cx).read(cx).listed_objects();
        assert_eq!(listed.len(), services + 1, "{listed:?}");
        assert_eq!(listed[0], ObjectKey::host("db-prod-03"));
        assert_eq!(
            dialog(app, cx).read(cx).submit_text(),
            format!("schedule {} downtimes", services + 1)
        );
        let flip = |app: &Harness, cx: &mut App, on: bool| {
            dialog(app, cx).update(cx, |dialog, cx| {
                dialog.edit_form(cx, |form| {
                    if let Form::Downtime(form) = form {
                        form.all_services = on;
                    }
                });
            });
            app.draw(cx);
        };
        let geometry = dialog(app, cx).read(cx).box_geometry();
        assert_eq!(geometry.0, services + 1);
        flip(app, cx, false);
        assert_eq!(
            dialog(app, cx).read(cx).listed_objects(),
            vec![ObjectKey::host("db-prod-03")]
        );
        assert_eq!(dialog(app, cx).read(cx).submit_text(), "schedule downtime");
        assert_eq!(
            dialog(app, cx).read(cx).box_geometry(),
            geometry,
            "switching all services off moves nothing"
        );
        flip(app, cx, true);
        type_into(app, cx, FormField::Comment, "kernel update");
        type_into(app, cx, FormField::End, "yesterday");
        app.keys(cx, "enter");
        let issues = dialog(app, cx).read(cx).shown_issues();
        assert!(issues.contains_key(&FormField::End), "{issues:?}");
        assert_eq!(
            focused(app, cx),
            Some(FormField::End),
            "the keyboard goes to the problem"
        );
        let dialog_entity = dialog(app, cx);
        app.in_window(cx, |window, cx| {
            dialog_entity.update(cx, |dialog, cx| {
                dialog.choose_preset(FormField::End, "+2h", window, cx);
            });
        });
        app.draw(cx);
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        let actions = recorder.actions();
        let [
            (
                _,
                target,
                Action::ScheduleDowntime {
                    start,
                    end,
                    mode,
                    all_services,
                    ..
                },
            ),
        ] = actions.as_slice()
        else {
            panic!("one downtime: {actions:?}");
        };
        assert_eq!(
            *target,
            ActionTarget::Objects(vec![ObjectKey::host("db-prod-03")])
        );
        assert!((end.as_unix_seconds() - start.as_unix_seconds() - 7_200.).abs() < 1.);
        assert_eq!(*mode, DowntimeMode::Fixed);
        assert!(*all_services, "a host's services by default");
    });
}

#[test]
fn a_downtime_is_triggered_by_one_picked_from_the_objects_downtimes() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        // A service of a host in downtime: the host's is offered.
        request(
            app,
            cx,
            ObjectAction::ScheduleDowntime,
            vec![ObjectKey::service("edge-fra-04", "load")],
        );
        let offers = dialog(app, cx).read(cx).trigger_offers().to_vec();
        assert_eq!(offers.len(), 1, "{offers:?}");
        assert_eq!(offers[0].name, "edge-fra-04!demo-downtime-2");
        assert!(
            offers[0]
                .label
                .starts_with("edge-fra-04 · rack maintenance · a.ivanova · "),
            "{}",
            offers[0].label
        );
        type_into(app, cx, FormField::Comment, "after the rack");
        let dialog_entity = dialog(app, cx);
        app.in_window(cx, |window, cx| {
            dialog_entity.update(cx, |dialog, cx| dialog.pick_trigger(0, window, cx));
        });
        app.draw(cx);
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        let actions = recorder.actions();
        let [(_, _, Action::ScheduleDowntime { trigger_name, .. })] = actions.as_slice() else {
            panic!("one downtime: {actions:?}");
        };
        assert_eq!(trigger_name.as_deref(), Some("edge-fra-04!demo-downtime-2"));
    });
}

#[test]
fn bulk_actions_apply_to_the_marked_rows_and_skip_what_icinga_refuses() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        // Shift-click marks a range (LIST-03); the selection bar appears
        // under the list without moving the rows.
        app.click(cx, row_position(0), Modifiers::default());
        app.click(cx, row_position(2), shift());
        let marked = app.marked(cx);
        assert_eq!(marked.len(), 3);
        assert!(app.dashboard(cx).read(cx).has_marks(cx));
        app.click(cx, row_position(4), super::secondary());
        assert_eq!(app.marked(cx).len(), 4, "ctrl/cmd-click adds a row");
        app.click(cx, row_position(4), super::secondary());
        assert_eq!(app.marked(cx), marked);

        // An OK service and an acknowledged one join the request.
        let ok = find(app, cx, |service| !service.is_problem());
        let acknowledged = find(app, cx, |service| {
            service.check.acknowledgement.is_acknowledged()
        });
        let mut targets = marked.clone();
        targets.push(ok.clone());
        targets.push(acknowledged.clone());
        request(app, cx, ObjectAction::Acknowledge, targets);
        let eligible = dialog(app, cx).read(cx).eligible().clone();
        assert_eq!(eligible.targets, marked, "the problems");
        assert_eq!(
            eligible
                .skipped
                .iter()
                .map(|skipped| &skipped.object)
                .collect::<Vec<_>>(),
            [&ok, &acknowledged]
        );
        type_into(app, cx, FormField::Comment, "storage incident");
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "one request for all of them");
        assert_eq!(actions[0].1, ActionTarget::Objects(marked.clone()));

        // Icinga refuses one of them: the toast names it and why, the
        // pane's buttons show it.
        let refused = marked[1].clone();
        app.state.update(cx, |state, cx| {
            state.apply(CoreEvent::ActionFinished {
                id: 1,
                outcome: ActionOutcome {
                    ok: 2,
                    failed: vec![(
                        refused.full_name(),
                        "Object is already acknowledged.".to_owned(),
                    )],
                    error: None,
                },
            });
            cx.notify();
        });
        app.draw(cx);
        let (tone, title, lines) = toasts(app, cx).remove(0);
        assert_eq!(tone, ToastTone::Partial);
        assert_eq!(title, "Acknowledged 2 of 3 services");
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].ends_with(": Object is already acknowledged."),
            "{}",
            lines[0]
        );
        let failure = app
            .state
            .read(cx)
            .action_failure(&refused)
            .cloned()
            .unwrap();
        assert_eq!(failure.action, ObjectAction::Acknowledge);
    });
}

#[test]
fn checks_go_at_once_and_many_ask_first() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.keys(cx, "j j r");
        assert_eq!(modal(app, cx), None, "a check needs no dialog");
        assert_eq!(
            recorder.actions(),
            [(
                1,
                ActionTarget::Objects(vec![replication()]),
                Action::CheckNow { force: true }
            )]
        );
        assert_eq!(
            app.state.read(cx).pending_label(&replication()),
            Some("checking…")
        );
    });
}

#[test]
fn checking_many_objects_asks_first() {
    run(FixtureOptions { generated_rows: 40 }, |app, cx| {
        let recorder = record(app, cx);
        // The generated rows' dashboard is selected: mark them all.
        app.keys(cx, "ctrl-a");
        let count = app.marked(cx).len();
        assert!(count > crate::operate::CHECK_CONFIRM_ABOVE, "{count}");
        app.keys(cx, "r");
        let Some(ModalKind::Confirm(confirmation)) = modal(app, cx) else {
            panic!("a confirmation: {:?}", modal(app, cx));
        };
        assert_eq!(confirmation.title, format!("Check {count} services now?"));
        assert!(!confirmation.danger);
        let Confirmed::Action(spec) = &confirmation.action else {
            panic!("an action");
        };
        assert_eq!(spec.objects.len(), count);
        assert!(recorder.actions().is_empty(), "not before confirming");
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        assert_eq!(recorder.actions().len(), 1, "one request for all");
    });
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one story: each removal asked for, listed and sent"
)]
fn acks_and_comments_go_at_once_downtimes_list_what_goes() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let acknowledged = find(app, cx, |service| {
            service.check.acknowledgement.is_acknowledged()
        });
        request(
            app,
            cx,
            ObjectAction::RemoveAcknowledgement,
            vec![acknowledged.clone()],
        );
        assert_eq!(modal(app, cx), None);
        assert_eq!(recorder.actions()[0].2, Action::RemoveAcknowledgement);
        assert_eq!(
            app.state.read(cx).pending_label(&acknowledged),
            Some("removing ack…")
        );

        // One comment by name (the pane's ×).
        let (object, comment) = {
            let snapshot = app.state.read(cx).snapshot().clone();
            let (object, comments) = snapshot
                .comments
                .iter()
                .find(|(_, comments)| {
                    comments
                        .iter()
                        .any(|comment| comment.kind == ic_model::CommentKind::User)
                })
                .map(|(object, comments)| (object.clone(), comments.clone()))
                .unwrap();
            let comment = comments
                .into_iter()
                .find(|comment| comment.kind == ic_model::CommentKind::User)
                .unwrap();
            (object, comment.name)
        };
        request(
            app,
            cx,
            ObjectAction::RemoveComments(vec![comment.clone()]),
            vec![object],
        );
        assert_eq!(modal(app, cx), None);
        assert_eq!(
            recorder.actions()[1].1,
            ActionTarget::Comments(vec![comment])
        );

        // Downtimes always ask, listing every downtime that goes (topic
        // 01): every downtime of an object...
        let with_downtime = ObjectKey::service("cache-02", "redis-memory");
        request(
            app,
            cx,
            ObjectAction::RemoveDowntimes,
            vec![with_downtime.clone()],
        );
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::RemoveDowntime))
        );
        assert_eq!(
            dialog(app, cx).read(cx).listed_objects(),
            vec![with_downtime.clone()]
        );
        assert_eq!(dialog(app, cx).read(cx).submit_text(), "remove downtime");
        app.keys(cx, "escape");
        assert_eq!(modal(app, cx), None);
        assert_eq!(recorder.actions().len(), 2, "not confirmed: not sent");
        request(
            app,
            cx,
            ObjectAction::RemoveDowntimes,
            vec![with_downtime.clone()],
        );
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        let actions = recorder.actions();
        assert_eq!(actions.len(), 3);
        // By the names listed, never by object: a downtime scheduled after
        // the dialog opened was never shown, and stays.
        let listed: Vec<String> = app.state.read(cx).snapshot().downtimes[&with_downtime]
            .iter()
            .map(|downtime| downtime.name.clone())
            .collect();
        assert_eq!(actions[2].1, ActionTarget::Downtimes(listed));
        assert_eq!(actions[2].2, Action::RemoveAllDowntimes);

        // ... and one by name (the banner's *remove downtime*).
        let name = app.state.read(cx).snapshot().downtimes[&with_downtime][0]
            .name
            .clone();
        request(
            app,
            cx,
            ObjectAction::RemoveDowntime(name.clone()),
            vec![with_downtime.clone()],
        );
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::RemoveDowntime))
        );
        assert_eq!(recorder.actions().len(), 3, "asked first");
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 4);
        assert_eq!(actions[3].1, ActionTarget::Downtimes(vec![name]));
        assert_eq!(actions[3].2, Action::RemoveAllDowntimes);
    });
}

/// Gives edge-fra-04's services the children Icinga makes for a host
/// downtime with `all services`; the host's downtime's name.
pub(super) fn host_downtime_with_its_services(app: &Harness, cx: &mut App) -> String {
    let host = ObjectKey::host("edge-fra-04");
    let mut parent = String::new();
    app.state.update(cx, |state, cx| {
        let old = state.snapshot().clone();
        let mut downtimes = (*old.downtimes).clone();
        let host_downtime = downtimes[&host][0].clone();
        parent.clone_from(&host_downtime.name);
        let mut services = (*old.services).clone();
        for service in services
            .values_mut()
            .filter(|service| service.key.host.as_str() == "edge-fra-04")
        {
            std::sync::Arc::make_mut(service).check.downtime_depth = 1;
            let object = service.object_key();
            downtimes
                .entry(object.clone())
                .or_default()
                .push(ic_model::Downtime {
                    name: format!("{}!child", object.full_name()),
                    object,
                    parent: Some(host_downtime.name.clone()),
                    ..host_downtime.clone()
                });
        }
        state.set_snapshot(std::sync::Arc::new(ic_core::snapshot::Snapshot {
            revision: old.revision + 1,
            downtimes: std::sync::Arc::new(downtimes),
            services: std::sync::Arc::new(services),
            ..(*old).clone()
        }));
        cx.notify();
    });
    app.draw(cx);
    parent
}

#[test]
fn every_downtime_case_renders_in_the_panes_and_tabs() {
    run(FixtureOptions::default(), |app, cx| {
        host_downtime_with_its_services(app, cx);
        let host = ObjectKey::host("edge-fra-04");
        let later = |minutes: f64| {
            ic_model::Timestamp::from_unix_seconds(
                ic_model::Timestamp::now().as_unix_seconds() + minutes * 60.,
            )
        };
        // Tonight's flexible one and a weekly one from the config on the
        // host; a fixed one still to come on a problem.
        let pg = replication();
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut downtimes = (*old.downtimes).clone();
            let base = downtimes[&host][0].clone();
            let list = downtimes.entry(host.clone()).or_default();
            list.push(ic_model::Downtime {
                name: "edge-fra-04!tonight".to_owned(),
                fixed: false,
                duration: 3_600.,
                start_time: later(480.),
                end_time: later(960.),
                trigger_time: None,
                in_effect: false,
                ..base.clone()
            });
            list.push(ic_model::Downtime {
                name: "edge-fra-04!weekly".to_owned(),
                start_time: later(3_000.),
                end_time: later(3_240.),
                trigger_time: None,
                in_effect: false,
                config_owned: true,
                schedule: Some("weekly-patching".to_owned()),
                author: "icingaadmin".to_owned(),
                ..base.clone()
            });
            downtimes.insert(
                pg.clone(),
                vec![ic_model::Downtime {
                    name: format!("{}!tonight", pg.full_name()),
                    object: pg.clone(),
                    start_time: later(470.),
                    end_time: later(530.),
                    trigger_time: None,
                    in_effect: false,
                    ..base
                }],
            );
            state.set_snapshot(std::sync::Arc::new(ic_core::snapshot::Snapshot {
                revision: old.revision + 1,
                downtimes: std::sync::Arc::new(downtimes),
                ..(*old).clone()
            }));
            cx.notify();
        });
        app.draw(cx);
        let snapshot = app.state.read(cx).snapshot().clone();
        let now = ic_model::Timestamp::now();
        let load = ObjectKey::service("edge-fra-04", "load");
        for object in [&host, &load, &pg] {
            let banner = crate::downtimes::banner(&snapshot, object, now)
                .unwrap_or_else(|| panic!("a banner for {object}"));
            assert_eq!(banner.in_effect, object != &pg, "{object}");
        }
        assert_eq!(
            crate::lists::threads::thread_of(&snapshot, &host, now).len(),
            3,
            "the banner shows one of three; the thread lists them all"
        );
        let dashboard = app.dashboard(cx);
        for object in [&host, &load, &pg] {
            dashboard.update(cx, |view, cx| view.open_object(object, cx));
            app.draw(cx);
            assert_eq!(app.pane_object(cx).as_ref(), Some(object));
            app.state.update(cx, |state, cx| {
                state.open_tab(object.clone());
                cx.notify();
            });
            app.draw(cx);
        }
        // Removing every downtime of the host: the config's is skipped,
        // the rest go by name (Icinga would stop at the config's).
        let recorder = record(app, cx);
        request(app, cx, ObjectAction::RemoveDowntimes, vec![host.clone()]);
        let listed = dialog(app, cx).read(cx).listed_objects();
        assert!(
            listed.contains(&host) && listed.contains(&load),
            "{listed:?}"
        );
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "one request for every name: {actions:?}");
        assert!(
            actions.iter().all(|(_, target, action)| {
                *action == Action::RemoveAllDowntimes
                    && matches!(target, ActionTarget::Downtimes(names)
                        if !names.is_empty() && !names.iter().any(|name| name == "edge-fra-04!weekly"))
            }),
            "{actions:?}"
        );
    });
}

#[test]
fn a_services_downtime_from_its_host_is_removed_whole_or_alone() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let parent = host_downtime_with_its_services(app, cx);
        let host = ObjectKey::host("edge-fra-04");
        let services = app
            .state
            .read(cx)
            .snapshot()
            .services_of(&ic_model::HostName::from("edge-fra-04"))
            .count();
        assert!(services > 1, "the fixture's host has services");
        let load = ObjectKey::service("edge-fra-04", "load");
        let child = format!("{}!child", load.full_name());
        request(
            app,
            cx,
            ObjectAction::RemoveDowntime(child.clone()),
            vec![load.clone()],
        );
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::RemoveDowntime))
        );
        // As drawn: the host's whole downtime, listing every downtime.
        let listed = dialog(app, cx).read(cx).listed_objects();
        assert_eq!(listed.len(), services + 1, "{listed:?}");
        assert!(listed.contains(&host) && listed.contains(&load));
        assert_eq!(
            dialog(app, cx).read(cx).submit_text(),
            format!("remove {} downtimes", services + 1)
        );
        // This service only.
        dialog(app, cx).update(cx, |dialog, cx| {
            dialog.edit_form(cx, |form| {
                if let Form::Removal(removal) = form {
                    removal.chosen = 0;
                }
            });
        });
        app.draw(cx);
        assert_eq!(
            dialog(app, cx).read(cx).listed_objects(),
            vec![load.clone()]
        );
        assert_eq!(dialog(app, cx).read(cx).submit_text(), "remove downtime");
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].1, ActionTarget::Downtimes(vec![child.clone()]));

        // The whole: the host's by name, Icinga removes its children.
        request(
            app,
            cx,
            ObjectAction::RemoveDowntime(child),
            vec![load.clone()],
        );
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert_eq!(actions[1].1, ActionTarget::Downtimes(vec![parent]));
        assert_eq!(actions[1].2, Action::RemoveAllDowntimes);
    });
}

/// Whether the keyboard is still inside the open action dialog.
fn dialog_has_focus(app: &Harness, cx: &mut App) -> bool {
    let dialog = dialog(app, cx);
    app.in_window(cx, |window, cx| {
        gpui::Focusable::focus_handle(dialog.read(cx), cx).contains_focused(window, cx)
    })
}

#[test]
fn the_removal_scope_goes_by_keyboard_and_tab_stays_in_the_dialog() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let parent = host_downtime_with_its_services(app, cx);
        let load = ObjectKey::service("edge-fra-04", "load");
        let child = format!("{}!child", load.full_name());
        let before = app
            .state
            .read(cx)
            .selected_dashboard()
            .map(|(_, dashboard)| dashboard.name.clone());
        request(
            app,
            cx,
            ObjectAction::RemoveDowntime(child.clone()),
            vec![load.clone()],
        );
        assert!(dialog_has_focus(app, cx));
        // Tab and Shift-Tab keep the keyboard in the dialog: nothing behind
        // it takes the keys that follow.
        app.keys(cx, "tab");
        assert!(dialog_has_focus(app, cx), "Tab stays in the dialog");
        app.keys(cx, "shift-tab j j j");
        assert!(dialog_has_focus(app, cx), "Shift-Tab stays in the dialog");
        assert_eq!(
            app.state
                .read(cx)
                .selected_dashboard()
                .map(|(_, dashboard)| dashboard.name.clone()),
            before
        );
        assert!(recorder.actions().is_empty());
        // ← / → choose the scope: this service only, then the whole again.
        let geometry = dialog(app, cx).read(cx).box_geometry();
        let whole = dialog(app, cx).read(cx).listed_objects().len();
        assert_eq!(geometry.0, whole, "sized for the widest scope");
        assert!(geometry.1.is_some(), "the button fits its longest label");
        app.keys(cx, "left");
        assert_eq!(
            dialog(app, cx).read(cx).listed_objects(),
            vec![load.clone()]
        );
        assert_eq!(
            dialog(app, cx).read(cx).box_geometry(),
            geometry,
            "the box and the button keep their size"
        );
        app.keys(cx, "right");
        assert!(dialog(app, cx).read(cx).listed_objects().len() > 1);
        app.keys(cx, "left enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].1, ActionTarget::Downtimes(vec![child.clone()]));
        // → past the last scope stays on it: the host and its services.
        request(
            app,
            cx,
            ObjectAction::RemoveDowntime(child.clone()),
            vec![load.clone()],
        );
        app.keys(cx, "left right right enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 2, "{actions:?}");
        assert_eq!(actions[1].1, ActionTarget::Downtimes(vec![parent]));
    });
}

#[test]
fn tab_in_a_dialog_without_fields_stays_in_it() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let host = ObjectKey::host("edge-fra-04");
        let before = app
            .state
            .read(cx)
            .selected_dashboard()
            .map(|(_, dashboard)| dashboard.name.clone());
        request(app, cx, ObjectAction::RemoveDowntimes, vec![host]);
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::RemoveDowntime))
        );
        app.keys(cx, "tab");
        assert!(dialog_has_focus(app, cx));
        app.keys(cx, "tab enter");
        assert_eq!(
            app.state
                .read(cx)
                .selected_dashboard()
                .map(|(_, dashboard)| dashboard.name.clone()),
            before
        );
        assert_eq!(modal(app, cx), None, "Enter removed, as asked");
        assert_eq!(recorder.actions().len(), 1);
    });
}

#[test]
fn nothing_to_do_and_refusals_say_why() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let ok = find(app, cx, |service| {
            service.state == ic_model::ServiceState::Ok
        });
        request(app, cx, ObjectAction::Acknowledge, vec![ok.clone()]);
        assert_eq!(modal(app, cx), None, "no empty dialog");
        let (tone, title, lines) = toasts(app, cx).remove(0);
        assert_eq!(tone, ToastTone::Info);
        assert_eq!(title, "Nothing to acknowledge");
        assert!(lines[0].ends_with(" is OK."), "{}", lines[0]);

        // An API user without action permissions (ENV-09).
        app.state.update(cx, |state, cx| {
            state.set_permissions(Some(ApiInfo {
                user: "viewer".to_owned(),
                permissions: vec!["objects/query/*".to_owned(), "events/*".to_owned()],
                version: "v2.15.6".to_owned(),
            }));
            cx.notify();
        });
        app.draw(cx);
        app.keys(cx, "j j a");
        assert_eq!(modal(app, cx), None);
        let (_, title, lines) = toasts(app, cx).pop().unwrap();
        assert_eq!(title, "Can't acknowledge");
        assert!(
            lines[0].contains("needs actions/acknowledge-problem"),
            "{}",
            lines[0]
        );
        assert!(recorder.actions().is_empty());

        // The palette lists the actions with the reason.
        app.keys(cx, "ctrl-k");
        let palette = app.workspace.read(cx).palette().cloned().unwrap();
        let items = palette.read(cx).items().to_vec();
        let ack = items
            .iter()
            .find(|item| {
                matches!(
                    item.command,
                    PaletteCommand::Act(ObjectAction::Acknowledge, _)
                )
            })
            .unwrap();
        assert!(
            ack.denied
                .as_deref()
                .is_some_and(|denial| denial.contains("viewer"))
        );
        app.keys(cx, "escape");
    });
}

#[test]
fn without_a_connection_the_dialog_keeps_what_was_typed() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.keys(cx, "j j c");
        assert_eq!(modal(app, cx), Some(ModalKind::Action(DialogKind::Comment)));
        type_into(app, cx, FormField::Comment, "replaced the disk");
        app.state.update(cx, |state, cx| {
            state.set_connection_lost();
            cx.notify();
        });
        app.draw(cx);
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), Some(ModalKind::Action(DialogKind::Comment)));
        assert_eq!(dialog(app, cx).read(cx).error(), Some(NOT_CONNECTED));
        let Form::Comment(form) = dialog(app, cx).read(cx).form().clone() else {
            panic!("the comment dialog");
        };
        assert_eq!(form.text, "replaced the disk", "nothing typed is lost");
        assert!(recorder.actions().is_empty());
    });
}

#[test]
fn running_a_command_asks_once_more() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        request(app, cx, ObjectAction::RunCommand, vec![replication()]);
        assert_eq!(modal(app, cx), Some(ModalKind::Action(DialogKind::Command)));
        assert_eq!(focused(app, cx), Some(FormField::Command));
        type_into(app, cx, FormField::Macros, "check_timeout = 30\nwarn = 400");
        // Enter in the macros field types a new line; it doesn't send.
        app.keys(cx, "tab tab");
        assert_eq!(focused(app, cx), Some(FormField::Macros));
        app.keys(cx, "enter");
        assert!(!dialog(app, cx).read(cx).is_confirming());
        assert!(dialog(app, cx).read(cx).shown_issues().is_empty());
        type_into(app, cx, FormField::Ttl, "0");
        app.keys(cx, "tab");
        assert_eq!(focused(app, cx), Some(FormField::Ttl));
        app.keys(cx, "enter");
        assert!(
            dialog(app, cx)
                .read(cx)
                .shown_issues()
                .contains_key(&FormField::Ttl)
        );
        assert_eq!(focused(app, cx), Some(FormField::Ttl));
        type_into(app, cx, FormField::Ttl, "10m");
        app.keys(cx, "enter");
        assert!(dialog(app, cx).read(cx).is_confirming(), "asks first");
        assert!(recorder.actions().is_empty());
        // Plain Enter doesn't confirm; ctrl/cmd-Enter does.
        app.keys(cx, "enter");
        assert!(recorder.actions().is_empty());
        app.keys(cx, "ctrl-enter");
        assert_eq!(modal(app, cx), None);
        let actions = recorder.actions();
        let [
            (
                _,
                _,
                Action::ExecuteCommand {
                    command_type,
                    command,
                    endpoint,
                    macros,
                    ttl,
                },
            ),
        ] = actions.as_slice()
        else {
            panic!("one command: {actions:?}");
        };
        assert_eq!(*command_type, CommandType::EventCommand);
        assert_eq!(*command, None);
        assert_eq!(*endpoint, None, "the core picks the object's endpoint");
        assert_eq!(macros["check_timeout"], serde_json::json!(30));
        assert!((ttl - 600.).abs() < f64::EPSILON);
    });
}

#[test]
fn passive_results_send_state_output_and_perfdata() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        request(
            app,
            cx,
            ObjectAction::SubmitCheckResult,
            vec![replication()],
        );
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::CheckResult))
        );
        assert_eq!(focused(app, cx), Some(FormField::Output));
        type_into(
            app,
            cx,
            FormField::Perfdata,
            "lag=3s;300;600 'wal kept'=1GB",
        );
        app.keys(cx, "tab");
        assert_eq!(focused(app, cx), Some(FormField::Perfdata));
        app.keys(cx, "shift-tab");
        type_into(app, cx, FormField::Output, "OK - caught up by hand");
        dialog(app, cx).update(cx, |dialog, cx| {
            dialog.edit_form(cx, |form| {
                if let Form::Result(form) = form {
                    form.exit_status = 0;
                }
            });
        });
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        assert_eq!(
            recorder.actions()[0].2,
            Action::ProcessCheckResult {
                exit_status: 0,
                output: "OK - caught up by hand".to_owned(),
                perfdata: vec!["lag=3s;300;600".to_owned(), "'wal kept'=1GB".to_owned()],
                ttl: None,
            }
        );
    });
}

#[test]
fn the_palette_copies_names_and_a_filter_expression() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j ctrl-k");
        let palette = app.workspace.read(cx).palette().cloned().unwrap();
        let items = palette.read(cx).items().to_vec();
        let copies: Vec<(&'static str, String)> = items
            .iter()
            .filter_map(|item| match &item.command {
                PaletteCommand::Copy { what, text } => Some((*what, text.clone())),
                _ => None,
            })
            .collect();
        // Further down the list than the first screen: search for them.
        assert!(copies.is_empty() || copies.len() == 2);
        app.keys(cx, "c o p y space f i l");
        let items = palette.read(cx).items().to_vec();
        let filter = items
            .iter()
            .find_map(|item| match &item.command {
                PaletteCommand::Copy {
                    what: "the filter expression",
                    text,
                } => Some(text.clone()),
                _ => None,
            })
            .expect("copy filter expression");
        assert_eq!(
            filter,
            "host.name == \"db-prod-03\" && service.name == \"postgres-replication\""
        );
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        assert_eq!(
            toasts(app, cx).pop().unwrap().1,
            "Copied the filter expression"
        );
    });
}

#[test]
fn shift_enter_starts_a_new_line_in_a_comment() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.keys(cx, "j j c");
        app.keys(cx, "o n e shift-enter t w o");
        assert_eq!(modal(app, cx), Some(ModalKind::Action(DialogKind::Comment)));
        let Form::Comment(form) = dialog(app, cx).read(cx).form().clone() else {
            panic!("the comment dialog");
        };
        assert_eq!(form.text, "one\ntwo");
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        assert_eq!(
            recorder.actions()[0].2,
            Action::AddComment {
                text: "one\ntwo".to_owned(),
                expiry: None,
            }
        );
    });
}

#[test]
fn the_panes_menu_offers_the_other_actions_and_copying() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "j j enter");
        let pane = app.dashboard(cx).read(cx).pane(cx).unwrap();
        let labels = pane.update(cx, |pane, cx| {
            pane.more_menu_labels(Some("CRITICAL".to_owned()), cx)
        });
        let morning = format!(
            "mute {}",
            crate::notifications::MuteChoice::UntilMorning.label(ic_model::Timestamp::now())
        );
        assert_eq!(
            labels,
            [
                "submit check result",
                "run command",
                // Watching and muting (NOTE-02).
                "watch: always notify",
                "mute for 1 hour",
                "mute for 4 hours",
                morning.as_str(),
                "mute until unmuted",
                "copy name",
                "copy filter expression",
                "copy output",
                // Its links, macros resolved (PANE-05).
                "open notes url",
                "open action url 1",
                "open action url 2",
            ]
        );
        pane.update(cx, ObjectPane::open_more_menu);
        app.draw(cx);
        assert_eq!(pane.read(cx).open_menu(), Some(PaneMenu::More));
        // An object with downtimes can lose them from there.
        let with_downtime = {
            let snapshot = app.state.read(cx).snapshot().clone();
            snapshot
                .downtimes
                .iter()
                .find(|(_, list)| !list.is_empty())
                .map(|(object, _)| object.clone())
                .unwrap()
        };
        pane.update(cx, |pane, cx| pane.show(with_downtime, cx));
        let labels = pane.update(cx, |pane, cx| pane.more_menu_labels(None, cx));
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("remove") && label.contains("downtime")),
            "{labels:?}"
        );
    });
}

/// NOTE-01: an action asked for while a dialog is open (a desktop
/// notification's Acknowledge while the palette is open) waits for it,
/// says so, and opens once the dialog is closed.
#[test]
fn a_request_behind_an_open_dialog_waits_for_it() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-k");
        assert_eq!(modal(app, cx), Some(ModalKind::Palette));
        request(app, cx, ObjectAction::Acknowledge, vec![replication()]);
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Palette),
            "the palette stays"
        );
        assert!(
            toasts(app, cx)
                .iter()
                .any(|(_, title, _)| title == "An action waits for this dialog"),
            "{:?}",
            toasts(app, cx)
        );
        app.keys(cx, "escape");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Action(DialogKind::Acknowledge))
        );
        assert_eq!(dialog(app, cx).read(cx).eligible().targets, [replication()]);
    });
}

/// `secondary-enter` on any verb of the palette (`ack`, `acknowledge`,
/// `downtime`, `dt`, `check`, `recheck`, `comment`, `note`) opens the
/// action's dialog listing every object the query names (*all N
/// matches*), and sends nothing: a loose query never acts directly, not
/// even a check, which goes at once from a row or a pane. The row shows a
/// several-objects mark in the dot's place.
#[test]
fn secondary_enter_on_any_verb_lists_every_target_in_a_dialog() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let verbs = [
            ("ack db-prod", DialogKind::Acknowledge),
            ("acknowledge db-prod", DialogKind::Acknowledge),
            ("downtime db-prod", DialogKind::Downtime),
            ("dt db-prod", DialogKind::Downtime),
            ("check db-prod", DialogKind::Check),
            ("recheck db-prod", DialogKind::Check),
            ("comment db-prod", DialogKind::Comment),
            ("note db-prod", DialogKind::Comment),
        ];
        for (query, kind) in verbs {
            app.keys(cx, "ctrl-k");
            let palette = app.workspace.read(cx).palette().unwrap().clone();
            let input = palette.read(cx).input().clone();
            app.in_window(cx, |window, cx| {
                input.update(cx, |input, cx| input.replace_all(query, window, cx));
            });
            app.draw(cx);
            let items = palette.read(cx).items().to_vec();
            let index = crate::palette::model::all_matches_item(&items)
                .unwrap_or_else(|| panic!("{query}: an all-matches item"));
            let all = &items[index];
            assert!(all.several, "{query}: the several-objects mark");
            assert!(all.dot.is_some(), "{query}: tinted by the worst state");
            let PaletteCommand::Act(_, targets) = &all.command else {
                panic!("{query}: {:?}", all.command);
            };
            assert!(targets.len() > 1, "{query}");

            app.keys(cx, "ctrl-enter");
            assert_eq!(
                modal(app, cx),
                Some(ModalKind::Action(kind)),
                "{query}: the dialog, not the action"
            );
            let dialog = dialog(app, cx);
            let eligible = dialog.read(cx).eligible();
            let mut listed: Vec<ObjectKey> = eligible
                .targets
                .iter()
                .cloned()
                .chain(
                    eligible
                        .skipped
                        .iter()
                        .map(|skipped| skipped.object.clone()),
                )
                .collect();
            let mut expected = targets.clone();
            listed.sort();
            expected.sort();
            assert_eq!(listed, expected, "{query}: every target is listed");
            assert!(recorder.actions().is_empty(), "{query}: nothing was sent");
            app.keys(cx, "escape");
            assert_eq!(modal(app, cx), None, "{query}");
        }

        // The check dialog sends one forced check for all of them once
        // confirmed (Enter).
        app.keys(cx, "ctrl-k");
        let palette = app.workspace.read(cx).palette().unwrap().clone();
        let input = palette.read(cx).input().clone();
        app.in_window(cx, |window, cx| {
            input.update(cx, |input, cx| {
                input.replace_all("check db-prod", window, cx);
            });
        });
        app.draw(cx);
        let items = palette.read(cx).items().to_vec();
        let index = crate::palette::model::all_matches_item(&items).unwrap();
        let PaletteCommand::Act(_, targets) = items[index].command.clone() else {
            panic!("an action");
        };
        app.keys(cx, "ctrl-enter");
        assert_eq!(modal(app, cx), Some(ModalKind::Action(DialogKind::Check)));
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None, "sent and closed");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "one request for all");
        assert_eq!(actions[0].1, ActionTarget::Objects(targets));
        assert_eq!(actions[0].2, Action::CheckNow { force: true });
    });
}
