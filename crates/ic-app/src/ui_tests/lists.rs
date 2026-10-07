//! The lists of every downtime, comment and acknowledged problem (topic
//! 07) in the window, on the fixture with a recording core: opening them
//! from the palette as tabs of the sidebar's `open` section, their rows and
//! counts, folding a host's downtime, marks, the removal dialogs listing
//! every target with a counted button, the one action they send,
//! permissions, *only mine*, live updates and virtualisation.

use std::sync::Arc;

use gpui::{App, Entity, Modifiers};
use ic_core::ApiInfo;
use ic_core::snapshot::Snapshot;
use ic_model::{Action, ActionTarget, ObjectKey};

use super::actions::{host_downtime_with_its_services, record, request, toasts};
use super::{Harness, row_position, run, secondary};
use crate::actions::ObjectAction;
use crate::fixture::FixtureOptions;
use crate::lists::dialog::RemovalDialog;
use crate::lists::model::{Line, RecordKey};
use crate::lists::{ListKind, RecordList};
use crate::palette::PaletteCommand;
use crate::workspace::ModalKind;

/// Opens `kind`'s list (as the palette does) and returns its view.
fn open(app: &Harness, cx: &mut App, kind: ListKind) -> Entity<RecordList> {
    app.state.update(cx, |state, cx| {
        state.open_list(kind);
        cx.notify();
    });
    app.draw(cx);
    list(app, cx, kind)
}

fn list(app: &Harness, cx: &App, kind: ListKind) -> Entity<RecordList> {
    app.workspace
        .read(cx)
        .list(kind)
        .cloned()
        .expect("the list's view")
}

fn modal(app: &Harness, cx: &App) -> Option<ModalKind> {
    app.workspace.read(cx).modal(cx)
}

fn removal_dialog(app: &Harness, cx: &App) -> Entity<RemovalDialog> {
    app.workspace
        .read(cx)
        .removal_dialog()
        .cloned()
        .expect("a removal dialog")
}

fn rows(app: &Harness, cx: &App, kind: ListKind) -> Vec<Line> {
    list(app, cx, kind).read(cx).rows()
}

/// The downtime's name on `object`.
fn downtime_name(app: &Harness, cx: &App, object: &ObjectKey) -> String {
    app.state.read(cx).snapshot().downtimes[object][0]
        .name
        .clone()
}

#[test]
fn lists_open_from_the_palette_as_tabs_before_the_objects() {
    run(FixtureOptions::default(), |app, cx| {
        for (kind, query) in [
            (ListKind::Downtimes, "d o w n t i m e s"),
            (ListKind::Acknowledged, "a c k n o w l e d g e d"),
        ] {
            app.keys(cx, "ctrl-k");
            app.keys(cx, query);
            let palette = app.workspace.read(cx).palette().cloned().unwrap();
            let items = palette.read(cx).items().to_vec();
            let item = items
                .iter()
                .find(|item| item.command == PaletteCommand::OpenList(kind))
                .unwrap_or_else(|| panic!("the palette opens the {} list", kind.title()));
            assert!(item.detail.starts_with("every "), "{}", item.detail);
            app.keys(cx, "escape");
        }
        app.keys(cx, "ctrl-k c o m m e n t s");
        let palette = app.workspace.read(cx).palette().cloned().unwrap();
        let first = palette.read(cx).items()[0].command.clone();
        assert_eq!(first, PaletteCommand::OpenList(ListKind::Comments));
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        assert_eq!(app.state.read(cx).lists(), &[ListKind::Comments]);
        assert_eq!(app.state.read(cx).active_list(), Some(ListKind::Comments));
        assert!(app.workspace.read(cx).list(ListKind::Comments).is_some());

        // A second list: tabs in a fixed order, the new one shown.
        open(app, cx, ListKind::Downtimes);
        assert_eq!(
            app.state.read(cx).lists(),
            &[ListKind::Downtimes, ListKind::Comments]
        );
        assert_eq!(app.state.read(cx).active_list(), Some(ListKind::Downtimes));

        // Opening an object's tab hides the list; the list stays open.
        let object = ObjectKey::service("cache-02", "redis-memory");
        app.state.update(cx, |state, cx| {
            state.open_tab(object.clone());
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(app.state.read(cx).active_list(), None);
        assert_eq!(app.state.read(cx).lists().len(), 2);

        // Closing the shown list's tab closes it, and drops its view.
        app.state.update(cx, |state, cx| {
            state.open_list(ListKind::Downtimes);
            cx.notify();
        });
        app.draw(cx);
        let list = list(app, cx, ListKind::Downtimes);
        app.click(cx, row_position(1), Modifiers::default());
        assert_eq!(list.read(cx).cursor(), Some(1));
        app.keys(cx, "escape secondary-w");
        assert_eq!(app.state.read(cx).lists(), &[ListKind::Comments]);
        assert!(app.workspace.read(cx).list(ListKind::Downtimes).is_none());
    });
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one walk through the list, as a user would"
)]
fn downtimes_fold_into_their_host_and_go_in_one_action() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let parent = host_downtime_with_its_services(app, cx);
        let host = ObjectKey::host("edge-fra-04");
        let redis = ObjectKey::service("cache-02", "redis-memory");
        let redis_downtime = downtime_name(app, cx, &redis);
        let list = open(app, cx, ListKind::Downtimes);

        // In effect, ending soonest first; the host's services folded into
        // its row.
        let lines = list.read(cx).rows();
        assert!(
            matches!(lines[0], Line::Section { count: 2, .. }),
            "{lines:?}"
        );
        let Line::Downtime { key, children, .. } = &lines[1] else {
            panic!("{lines:?}");
        };
        assert_eq!(key, &RecordKey::Downtime(parent.clone()));
        let services = children.services;
        assert!(services > 0);
        assert!(matches!(&lines[2], Line::Downtime { object, .. } if *object == redis));
        assert_eq!(lines.len(), 3);
        let summary = list.read(cx).summary().unwrap();
        assert_eq!((summary.in_effect, summary.upcoming), (2, 0));
        assert_eq!(summary.folded, services);

        // A click puts the cursor on the host's downtime and opens the host.
        app.click(cx, row_position(1), Modifiers::default());
        assert_eq!(list.read(cx).cursor(), Some(1));
        assert_eq!(list.read(cx).pane_object(cx), Some(host.clone()));
        app.keys(cx, "escape");
        assert_eq!(list.read(cx).pane_object(cx), None);

        // Right unfolds the services under it, left folds them back.
        app.keys(cx, "right");
        assert_eq!(rows(app, cx, ListKind::Downtimes).len(), 3 + services);
        assert!(
            rows(app, cx, ListKind::Downtimes)[2..2 + services]
                .iter()
                .all(|line| matches!(line, Line::Downtime { child: true, .. }))
        );
        app.keys(cx, "left");
        assert_eq!(rows(app, cx, ListKind::Downtimes).len(), 3);

        // Marks: the host's downtime and redis's.
        app.keys(cx, "x j x");
        assert_eq!(
            list.read(cx).marked(),
            vec![
                RecordKey::Downtime(parent.clone()),
                RecordKey::Downtime(redis_downtime.clone())
            ]
        );

        // Backspace asks, listing every downtime that goes.
        app.keys(cx, "backspace");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Removal(ListKind::Downtimes))
        );
        let dialog = removal_dialog(app, cx);
        let removal = dialog.read(cx).removal().clone();
        assert_eq!(removal.selected, 2);
        assert_eq!(removal.count, 2 + services);
        assert_eq!(
            removal.button(),
            format!("remove {} downtimes", 2 + services)
        );
        let objects = removal.objects();
        assert_eq!(objects.len(), 2 + services);
        assert!(objects.contains(&host) && objects.contains(&redis));
        assert!(
            removal
                .consequence
                .as_deref()
                .is_some_and(|text| text.contains("redis-memory on cache-02")),
            "{:?}",
            removal.consequence
        );
        assert!(recorder.actions().is_empty(), "nothing before Enter");

        // Escape: nothing sent, the marks stay.
        app.keys(cx, "escape");
        assert_eq!(modal(app, cx), None);
        assert!(recorder.actions().is_empty());
        assert_eq!(list.read(cx).marked().len(), 2);

        // Enter sends one action naming the downtimes.
        app.keys(cx, "backspace enter");
        assert_eq!(modal(app, cx), None);
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "{actions:?}");
        let ActionTarget::Downtimes(names) = &actions[0].1 else {
            panic!("{:?}", actions[0].1);
        };
        assert!(names.contains(&parent) && names.contains(&redis_downtime));
        assert_eq!(actions[0].2, Action::RemoveAllDowntimes);
        assert!(app.state.read(cx).pending_label(&redis).is_some());

        // The event stream says redis's downtime is gone: its row goes.
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut downtimes = (*old.downtimes).clone();
            downtimes.remove(&redis);
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                downtimes: Arc::new(downtimes),
                ..(*old).clone()
            }));
            cx.notify();
        });
        app.draw(cx);
        let lines = rows(app, cx, ListKind::Downtimes);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(
            list.read(cx).marked(),
            vec![RecordKey::Downtime(parent)],
            "the gone row's mark goes with it"
        );
    });
}

#[test]
fn removing_comments_skips_acknowledgements_and_says_so() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let list = open(app, cx, ListKind::Comments);
        let lines = list.read(cx).rows();
        assert_eq!(lines.len(), 3, "{lines:?}");
        let summary = list.read(cx).summary().unwrap();
        assert_eq!((summary.comments, summary.acknowledgements), (1, 2));

        app.click(cx, row_position(0), Modifiers::default());
        app.keys(cx, "escape secondary-a");
        assert_eq!(list.read(cx).marked().len(), 3);
        app.keys(cx, "backspace");
        assert_eq!(modal(app, cx), Some(ModalKind::Removal(ListKind::Comments)));
        let removal = removal_dialog(app, cx).read(cx).removal().clone();
        assert_eq!(removal.count, 1);
        assert_eq!(removal.button(), "remove comment", "one: no count");
        assert_eq!(removal.what(), "3 selected · 1 comment");
        assert!(
            removal
                .skipped
                .iter()
                .any(|line| line.contains("acknowledgement")),
            "{:?}",
            removal.skipped
        );
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1);
        let comment = app.state.read(cx).snapshot().comments
            [&ObjectKey::service("db-prod-03", "postgres-replication")][0]
            .name
            .clone();
        assert_eq!(actions[0].1, ActionTarget::Comments(vec![comment]));
    });
}

#[test]
fn acknowledgements_go_from_the_list_and_from_a_dashboard_after_a_listing() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let list = open(app, cx, ListKind::Acknowledged);
        let lines = list.read(cx).rows();
        assert!(lines.len() >= 2, "{lines:?}");
        let objects: Vec<ObjectKey> = lines
            .iter()
            .map(|line| match line {
                Line::Problem {
                    key: RecordKey::Problem(object),
                } => object.clone(),
                other => panic!("{other:?}"),
            })
            .collect();
        app.click(cx, row_position(0), Modifiers::default());
        app.keys(cx, "escape secondary-a backspace");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Removal(ListKind::Acknowledged))
        );
        let removal = removal_dialog(app, cx).read(cx).removal().clone();
        assert_eq!(removal.count, objects.len());
        assert_eq!(
            removal.button(),
            format!("remove {} acknowledgements", objects.len())
        );
        assert_eq!(removal.objects(), objects);
        app.keys(cx, "enter");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].2, Action::RemoveAcknowledgement);
        let ActionTarget::Objects(sent) = &actions[0].1 else {
            panic!("{:?}", actions[0].1);
        };
        assert_eq!(sent.len(), objects.len());

        // From a dashboard, several acknowledgements ask with the same
        // listing; one goes at once.
        recorder.clear();
        request(
            app,
            cx,
            ObjectAction::RemoveAcknowledgement,
            objects.clone(),
        );
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Removal(ListKind::Acknowledged))
        );
        app.keys(cx, "escape");
        assert!(recorder.actions().is_empty());
    });
}

#[test]
fn permissions_disable_removal_and_hide_what_cant_be_read() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        app.state.update(cx, |state, cx| {
            state.set_permissions(Some(ApiInfo {
                user: "viewer".to_owned(),
                permissions: vec!["objects/query/*".to_owned(), "events/*".to_owned()],
                version: "v2.15.6".to_owned(),
            }));
            cx.notify();
        });
        let list = open(app, cx, ListKind::Downtimes);
        app.click(cx, row_position(1), Modifiers::default());
        app.keys(cx, "escape x backspace");
        assert_eq!(modal(app, cx), None, "no dialog for what can't be sent");
        let (_, title, lines) = toasts(app, cx).pop().unwrap();
        assert_eq!(title, "Can't remove downtimes");
        assert!(lines[0].contains("viewer"), "{}", lines[0]);
        assert!(recorder.actions().is_empty());
        assert_eq!(list.read(cx).marked().len(), 1);

        // Without objects/query/Comment the comment list says why instead
        // of showing nothing as if there were none.
        app.state.update(cx, |state, cx| {
            state.set_permissions(Some(ApiInfo {
                user: "viewer".to_owned(),
                permissions: vec![
                    "objects/query/Host".to_owned(),
                    "objects/query/Service".to_owned(),
                ],
                version: "v2.15.6".to_owned(),
            }));
            cx.notify();
        });
        let state = app.state.read(cx);
        assert!(
            state
                .list_denial(ListKind::Comments)
                .is_some_and(|denial| denial.contains("objects/query/Comment"))
        );
        assert!(state.list_denial(ListKind::Downtimes).is_some());
        assert_eq!(state.list_denial(ListKind::Acknowledged), None);
    });
}

#[test]
fn only_mine_hides_what_others_set() {
    run(FixtureOptions::default(), |app, cx| {
        let list = open(app, cx, ListKind::Downtimes);
        assert_eq!(app.state.read(cx).author(), "icygui");
        list.update(cx, |list, cx| {
            list.edit_options(cx, |options| options.only_mine = true);
        });
        app.draw(cx);
        assert!(list.read(cx).rows().is_empty());
        assert_eq!(list.read(cx).summary().unwrap().by_others, 2);

        // Set by the environment's author: shown.
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut downtimes = (*old.downtimes).clone();
            let redis = ObjectKey::service("cache-02", "redis-memory");
            downtimes.get_mut(&redis).unwrap()[0].author = "icygui".to_owned();
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                downtimes: Arc::new(downtimes),
                ..(*old).clone()
            }));
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(list.read(cx).rows().len(), 2, "the section and redis's");
        assert_eq!(list.read(cx).summary().unwrap().by_others, 1);
    });
}

#[test]
fn thousands_of_downtimes_build_only_the_rows_on_screen() {
    run(FixtureOptions::default(), |app, cx| {
        let redis = ObjectKey::service("cache-02", "redis-memory");
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut downtimes = (*old.downtimes).clone();
            let template = downtimes[&redis][0].clone();
            let many = downtimes.get_mut(&redis).unwrap();
            for index in 0..3_000 {
                many.push(ic_model::Downtime {
                    name: format!("{}!many-{index}", redis.full_name()),
                    ..template.clone()
                });
            }
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                downtimes: Arc::new(downtimes),
                ..(*old).clone()
            }));
            cx.notify();
        });
        let list = open(app, cx, ListKind::Downtimes);
        assert_eq!(list.read(cx).rows().len(), 1 + 3_002);
        let visible = list.read(cx).visible_rows();
        assert!(visible.len() < 40, "{visible:?}");
        app.click(cx, row_position(1), Modifiers::default());
        app.keys(cx, "escape end");
        assert_eq!(list.read(cx).cursor(), Some(3_002));
        let visible = list.read(cx).visible_rows();
        assert!(
            visible.contains(&3_002) && visible.len() < 40,
            "{visible:?}"
        );

        // A ctrl-click marks a row.
        app.keys(cx, "home");
        app.click(cx, row_position(2), secondary());
        assert_eq!(list.read(cx).marked().len(), 1);

        // The selection bar's buttons stay put as the count grows (its
        // slot fits `999 selected`, as drawn).
        let buttons_x = |cx: &App| list.read(cx).selection_buttons_x.get().unwrap();
        let one = buttons_x(cx);
        for _ in 0..9 {
            app.keys(cx, "shift-j");
        }
        assert_eq!(list.read(cx).marked().len(), 10);
        assert_eq!(buttons_x(cx), one, "the buttons didn't move");
        for _ in 0..90 {
            app.keys(cx, "shift-j");
        }
        assert_eq!(list.read(cx).marked().len(), 100);
        assert_eq!(buttons_x(cx), one, "the buttons didn't move");
        app.keys(cx, "secondary-a");
        assert_eq!(list.read(cx).marked().len(), 3_002);
    });
}
