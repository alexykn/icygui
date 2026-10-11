//! The handling and downtimes views (topic 14) in the window, on the
//! fixture with a recording core: opening them from the palette as tabs of
//! the sidebar's `open` section (*acknowledged* and *comments* open
//! handling on their chip), a thread per object under one band, the
//! chevron that only folds, a host's downtime folding its services, the
//! timeline and the list, marks over entries of every kind, the removal
//! dialog listing every target with a counted button and the actions it
//! sends, permissions, *only mine*, virtualisation, the choices kept, and
//! the pane's thread with its comment field.

use std::sync::Arc;

use gpui::{App, Entity, Modifiers, Pixels, Point, point, px};
use ic_core::ApiInfo;
use ic_core::snapshot::Snapshot;
use ic_model::{Action, ActionTarget, ObjectKey};
use ic_ui_kit::Metrics;

use super::actions::{host_downtime_with_its_services, record, request, toasts};
use super::{Harness, run, secondary};
use crate::actions::ObjectAction;
use crate::cluster::ClusterEntry;
use crate::fixture::FixtureOptions;
use crate::lists::dialog::RemovalDialog;
use crate::lists::model::{Chip, Mode, SortChoice};
use crate::lists::removal::RemovalKind;
use crate::lists::threads::{EntryKey, ItemKey, Line, Section};
use crate::lists::{ListKind, RecordList};
use crate::palette::PaletteCommand;
use crate::workspace::ModalKind;

/// Opens `kind`'s view (as the palette does) and returns it.
pub(super) fn open(app: &Harness, cx: &mut App, kind: ListKind) -> Entity<RecordList> {
    app.state.update(cx, |state, cx| {
        state.open_list(kind);
        cx.notify();
    });
    app.draw(cx);
    view(app, cx, kind)
}

fn view(app: &Harness, cx: &App, kind: ListKind) -> Entity<RecordList> {
    app.workspace
        .read(cx)
        .list(kind)
        .cloned()
        .expect("the view")
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

pub(super) fn lines(view: &Entity<RecordList>, cx: &App) -> Vec<Line> {
    view.read(cx).lines()
}

/// The line `index`'s middle on screen, away from the chevron (the view
/// scrolled to the top): below the header and the chips bar, right of
/// the 300px sidebar.
pub(super) fn line_position(view: &Entity<RecordList>, cx: &App, index: usize) -> Point<Pixels> {
    let metrics = ic_ui_kit::Theme::dark().metrics;
    let top =
        Metrics::with_rule(metrics.header_height) + Metrics::with_rule(metrics.summary_bar_height);
    let middle = view.read(cx).line_middle(index).expect("laid out");
    point(px(700.), top + middle)
}

/// A band's chevron, at its left.
fn chevron_position(view: &Entity<RecordList>, cx: &App, index: usize) -> Point<Pixels> {
    point(px(321.), line_position(view, cx, index).y)
}

/// Where `object`'s band is.
pub(super) fn band_of(lines: &[Line], object: &ObjectKey) -> Option<usize> {
    lines
        .iter()
        .position(|line| matches!(line, Line::Band { object: band, .. } if band == object))
}

/// Where the entry keyed `key` is.
fn entry_at(lines: &[Line], key: &EntryKey) -> Option<usize> {
    lines
        .iter()
        .position(|line| line.entry().is_some_and(|entry| entry.key == *key))
}

fn entries(lines: &[Line]) -> usize {
    lines.iter().filter(|line| line.entry().is_some()).count()
}

/// The downtime's name on `object`.
fn downtime_name(app: &Harness, cx: &App, object: &ObjectKey) -> String {
    app.state.read(cx).snapshot().downtimes[object][0]
        .name
        .clone()
}

fn replication() -> ObjectKey {
    ObjectKey::service("db-prod-03", "postgres-replication")
}

/// Types `query` in the palette and returns its first command.
fn first_command(app: &Harness, cx: &mut App, query: &str) -> PaletteCommand {
    app.keys(cx, "ctrl-k");
    app.keys(cx, query);
    let palette = app.workspace.read(cx).palette().cloned().unwrap();
    palette.read(cx).items()[0].command.clone()
}

#[test]
fn handling_and_downtimes_open_from_the_palette_as_tabs() {
    run(FixtureOptions::default(), |app, cx| {
        // Handling and downtimes, with what they hold.
        for (query, command, detail) in [
            (
                "h a n d l i n g",
                PaletteCommand::OpenList(ListKind::Handling, Some(Chip::All)),
                "who is handling what · ",
            ),
            (
                "d o w n t i m e s",
                PaletteCommand::OpenList(ListKind::Downtimes, None),
                "in effect and upcoming · 2 in effect",
            ),
        ] {
            app.keys(cx, "ctrl-k");
            app.keys(cx, query);
            let palette = app.workspace.read(cx).palette().cloned().unwrap();
            let items = palette.read(cx).items().to_vec();
            let item = items
                .iter()
                .find(|item| item.command == command)
                .unwrap_or_else(|| panic!("the palette opens {query}"));
            assert!(item.detail.starts_with(detail), "{}", item.detail);
            app.keys(cx, "escape");
        }

        // *acknowledged* opens handling on its chip: expiring soonest
        // first, those that never expire last.
        assert_eq!(
            first_command(app, cx, "a c k n o w l e d g e d"),
            PaletteCommand::OpenList(ListKind::Handling, Some(Chip::Acknowledged))
        );
        app.keys(cx, "enter");
        assert_eq!(modal(app, cx), None);
        assert_eq!(
            app.state.read(cx).active_cluster(),
            Some(ClusterEntry::Handling)
        );
        let handling = view(app, cx, ListKind::Handling);
        let options = handling.read(cx).current_options(cx);
        assert_eq!(options.chip, Chip::Acknowledged);
        assert_eq!(options.sort(ListKind::Handling), SortChoice::Soonest);
        let shown = lines(&handling, cx);
        assert_eq!(
            shown[0],
            Line::Section {
                section: Section::NoExpiry,
                count: 2
            },
            "{shown:#?}"
        );
        assert!(band_of(&shown, &ObjectKey::host("sw-core-ams-02")).is_some());
        assert!(band_of(&shown, &ObjectKey::service("web-edge-02", "http-tls")).is_some());
        assert_eq!(entries(&shown), 2, "only the acknowledgements");

        // *comments* shows the same view on its chip: one tab.
        assert_eq!(
            first_command(app, cx, "c o m m e n t s"),
            PaletteCommand::OpenList(ListKind::Handling, Some(Chip::Comments))
        );
        app.keys(cx, "enter");
        assert_eq!(
            app.state.read(cx).active_cluster(),
            Some(ClusterEntry::Handling),
            "the same view, on its chip"
        );
        let options = handling.read(cx).current_options(cx);
        assert_eq!(options.chip, Chip::Comments);
        assert_eq!(options.sort(ListKind::Handling), SortChoice::LatestActivity);
        let shown = lines(&handling, cx);
        assert_eq!(entries(&shown), 1, "{shown:#?}");
        assert!(band_of(&shown, &replication()).is_some());

        // The other entry of the cluster section shows instead.
        open(app, cx, ListKind::Downtimes);
        assert_eq!(
            app.state.read(cx).active_cluster(),
            Some(ClusterEntry::Downtimes)
        );

        // Opening an object's tab shows it instead; the entry stays in the
        // sidebar (the cluster section is always there).
        let object = ObjectKey::service("cache-02", "redis-memory");
        app.state.update(cx, |state, cx| {
            state.open_tab(object.clone());
            cx.notify();
        });
        app.draw(cx);
        assert_eq!(app.state.read(cx).active_cluster(), None);
        assert_eq!(app.state.read(cx).active_tab(), Some(&object));
        let downtimes = open(app, cx, ListKind::Downtimes);
        app.click(cx, line_position(&downtimes, cx, 1), Modifiers::default());
        assert_eq!(downtimes.read(cx).cursor(), Some(1));
    });
}

#[test]
fn handling_is_a_thread_per_object_and_the_chevron_only_folds() {
    run(FixtureOptions::default(), |app, cx| {
        let handling = open(app, cx, ListKind::Handling);
        let shown = lines(&handling, cx);
        // One band per object, each followed by its entries.
        let bands: Vec<&ObjectKey> = shown
            .iter()
            .filter_map(|line| match line {
                Line::Band { object, .. } => Some(object),
                _ => None,
            })
            .collect();
        assert_eq!(bands.len(), 5, "{shown:#?}");
        let unique: std::collections::HashSet<&&ObjectKey> = bands.iter().collect();
        assert_eq!(unique.len(), bands.len(), "the same object never twice");
        assert_eq!(entries(&shown), 5);
        assert!(matches!(shown[0], Line::Band { .. }), "no sections");
        let summary = handling.read(cx).summary().unwrap();
        assert_eq!(summary.count(Chip::Acknowledged), Some(2));
        assert_eq!(summary.count(Chip::InEffect), Some(2));
        assert_eq!(summary.count(Chip::Upcoming), Some(0));
        assert_eq!(summary.count(Chip::Comments), Some(1));
        assert_eq!(summary.objects, 5);
        // Latest activity first: the switch acknowledged 9 minutes ago.
        assert_eq!(bands[0], &ObjectKey::host("sw-core-ams-02"));
        // The band says what the thread holds.
        let band = band_of(&shown, &ObjectKey::host("sw-core-ams-02")).unwrap();
        let Line::Band { slot, .. } = &shown[band] else {
            unreachable!()
        };
        assert_eq!(slot, "acknowledged");

        // The chevron folds the thread to its band, and nothing else.
        let band = band_of(&shown, &replication()).unwrap();
        app.click(
            cx,
            chevron_position(&handling, cx, band),
            Modifiers::default(),
        );
        let folded = lines(&handling, cx);
        assert_eq!(folded.len(), shown.len() - 1);
        assert!(matches!(
            folded[band],
            Line::Band {
                collapsed: true,
                ..
            }
        ));
        assert_eq!(handling.read(cx).pane_object(cx), None, "no pane");
        assert_eq!(handling.read(cx).cursor(), None, "no cursor");
        app.click(
            cx,
            chevron_position(&handling, cx, band),
            Modifiers::default(),
        );
        assert_eq!(lines(&handling, cx), shown);

        // The rest of the band opens the object in the pane.
        app.click(cx, line_position(&handling, cx, band), Modifiers::default());
        assert_eq!(handling.read(cx).cursor(), Some(band));
        assert_eq!(handling.read(cx).pane_object(cx), Some(replication()));
        // So does its entry.
        app.click(
            cx,
            line_position(&handling, cx, band + 1),
            Modifiers::default(),
        );
        assert_eq!(handling.read(cx).cursor(), Some(band + 1));
        assert_eq!(handling.read(cx).pane_object(cx), Some(replication()));

        // Left in a thread folds it, the cursor on its band; right opens
        // it again.
        app.keys(cx, "left");
        assert_eq!(handling.read(cx).cursor(), Some(band));
        assert!(matches!(
            lines(&handling, cx)[band],
            Line::Band {
                collapsed: true,
                ..
            }
        ));
        app.keys(cx, "right");
        assert_eq!(lines(&handling, cx), shown);

        // Every object folded from the menu, unfolded again; the cursor
        // in a thread goes to its band.
        app.click(
            cx,
            line_position(&handling, cx, band + 1),
            Modifiers::default(),
        );
        handling.update(cx, |view, cx| view.fold_every_object(true, cx));
        app.draw(cx);
        let folded = lines(&handling, cx);
        assert_eq!(folded.len(), 5, "the bands only");
        assert_eq!(handling.read(cx).cursor(), band_of(&folded, &replication()));
        handling.update(cx, |view, cx| view.fold_every_object(false, cx));
        app.draw(cx);
        assert_eq!(lines(&handling, cx), shown);

        // A chip keeps every band's words: what the thread holds.
        handling.update(cx, |view, cx| {
            view.edit_options(cx, |options| options.pick_chip(Chip::InEffect));
        });
        app.draw(cx);
        let in_effect = lines(&handling, cx);
        assert_eq!(entries(&in_effect), 2, "{in_effect:#?}");
        assert!(in_effect.iter().all(|line| match line {
            Line::Entry { entry, .. } => matches!(entry.key, EntryKey::Downtime(_)),
            _ => true,
        }));
    });
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one walk through the view, as a user would"
)]
fn downtimes_fold_a_hosts_services_and_go_in_one_action() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let parent = host_downtime_with_its_services(app, cx);
        let host = ObjectKey::host("edge-fra-04");
        let redis = ObjectKey::service("cache-02", "redis-memory");
        let redis_downtime = downtime_name(app, cx, &redis);
        let services = app
            .state
            .read(cx)
            .snapshot()
            .services
            .keys()
            .filter(|key| key.host.as_str() == "edge-fra-04")
            .count();
        assert!(services > 7, "enough to page");
        let downtimes = open(app, cx, ListKind::Downtimes);

        // The timeline first: the axis, then one line per downtime; the
        // host's services folded into its downtime.
        let shown = lines(&downtimes, cx);
        assert_eq!(shown[0], Line::Axis, "{shown:#?}");
        let summary = downtimes.read(cx).summary().unwrap();
        assert_eq!(summary.downtimes, 2);
        assert_eq!(summary.folded, services);
        let Some(Line::Entry { single: false, .. }) =
            entry_at(&shown, &EntryKey::Downtime(parent.clone())).map(|at| &shown[at])
        else {
            panic!("the host's downtime under its band: {shown:#?}");
        };
        assert!(matches!(
            shown[entry_at(&shown, &EntryKey::Downtime(redis_downtime.clone())).unwrap()],
            Line::Entry { single: true, .. }
        ));

        // The list: in effect, ending soonest first; a lone downtime is
        // one row, the host's a band with its downtime and the fold.
        downtimes.update(cx, |view, cx| {
            view.edit_options(cx, |options| options.pick_mode(Mode::List));
        });
        app.draw(cx);
        let shown = lines(&downtimes, cx);
        assert_eq!(
            shown[0],
            Line::Section {
                section: Section::InEffect,
                count: 2
            },
            "{shown:#?}"
        );
        assert!(
            matches!(&shown[1], Line::Band { object, covers: Some(covers), .. }
            if *object == host && covers.services == services && covers.in_effect)
        );
        assert!(matches!(&shown[2], Line::Entry { entry, .. }
            if entry.key == EntryKey::Downtime(parent.clone())));
        assert!(matches!(&shown[3], Line::Fold { count, open: false, .. }
            if *count == services));
        assert!(matches!(&shown[4], Line::Entry { single: true, entry, .. }
            if entry.object == redis));
        assert_eq!(shown.len(), 5);

        // A click on the band opens the host.
        app.click(cx, line_position(&downtimes, cx, 1), Modifiers::default());
        assert_eq!(downtimes.read(cx).cursor(), Some(1));
        assert_eq!(downtimes.read(cx).pane_object(cx), Some(host.clone()));
        app.keys(cx, "escape");
        assert_eq!(downtimes.read(cx).pane_object(cx), None);

        // A click on the fold opens it: seven services, then `+ N more`.
        app.click(cx, line_position(&downtimes, cx, 3), Modifiers::default());
        let open_fold = lines(&downtimes, cx);
        assert!(matches!(open_fold[3], Line::Fold { open: true, .. }));
        let listed = open_fold
            .iter()
            .filter(|line| matches!(line, Line::Service { .. }))
            .count();
        assert_eq!(listed, 7, "{open_fold:#?}");
        assert!(matches!(open_fold[11], Line::More { hidden, .. } if hidden == services - 7));
        // Right on `+ N more` shows them all.
        app.keys(cx, "j j j j j j j j");
        assert_eq!(downtimes.read(cx).cursor(), Some(11));
        app.keys(cx, "right");
        let all = lines(&downtimes, cx);
        assert_eq!(
            all.iter()
                .filter(|line| matches!(line, Line::Service { .. }))
                .count(),
            services
        );
        // Left on the fold closes it.
        app.click(cx, line_position(&downtimes, cx, 3), Modifiers::default());
        assert_eq!(lines(&downtimes, cx).len(), 5);

        // Marks: the host's downtime and redis's.
        app.click(cx, line_position(&downtimes, cx, 2), secondary());
        app.click(cx, line_position(&downtimes, cx, 4), secondary());
        assert_eq!(
            downtimes.read(cx).marked(),
            vec![
                ItemKey::Entry(EntryKey::Downtime(parent.clone())),
                ItemKey::Entry(EntryKey::Downtime(redis_downtime.clone()))
            ]
        );

        // Backspace asks, listing every downtime that goes.
        app.keys(cx, "backspace");
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Removal(RemovalKind::Downtimes))
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
        assert_eq!(downtimes.read(cx).marked().len(), 2);

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
        let shown = lines(&downtimes, cx);
        assert_eq!(shown.len(), 4, "{shown:#?}");
        assert_eq!(
            downtimes.read(cx).marked(),
            vec![ItemKey::Entry(EntryKey::Downtime(parent))],
            "the gone row's mark goes with it"
        );
    });
}

#[test]
fn handling_removes_every_kind_in_one_confirmation() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let handling = open(app, cx, ListKind::Handling);
        app.click(cx, line_position(&handling, cx, 1), Modifiers::default());
        app.keys(cx, "escape secondary-a");
        // Marks go on entries only, never on the bands.
        assert_eq!(handling.read(cx).marked().len(), 5);
        assert!(
            handling
                .read(cx)
                .marked()
                .iter()
                .all(|key| matches!(key, ItemKey::Entry(_)))
        );
        app.keys(cx, "backspace");
        assert_eq!(modal(app, cx), Some(ModalKind::Removal(RemovalKind::Mixed)));
        let removal = removal_dialog(app, cx).read(cx).removal().clone();
        assert_eq!(removal.count, 5);
        assert_eq!(removal.button(), "remove all 5");
        assert_eq!(removal.objects().len(), 5, "every target listed");
        // Tab keeps the keyboard in the confirmation: the keys that follow
        // don't reach the view or the sidebar behind it.
        app.keys(cx, "tab shift-tab j j");
        assert_eq!(modal(app, cx), Some(ModalKind::Removal(RemovalKind::Mixed)));
        let dialog = removal_dialog(app, cx);
        assert!(app.in_window(cx, |window, cx| {
            gpui::Focusable::focus_handle(dialog.read(cx), cx).is_focused(window)
        }));
        assert_eq!(handling.read(cx).marked().len(), 5);
        app.keys(cx, "enter");

        // One action per kind: the downtimes, the acknowledgements, the
        // comment.
        let actions = recorder.actions();
        assert_eq!(actions.len(), 3, "{actions:?}");
        let comment = app.state.read(cx).snapshot().comments[&replication()][0]
            .name
            .clone();
        assert!(
            actions
                .iter()
                .any(|action| action.1 == ActionTarget::Comments(vec![comment.clone()]))
        );
        assert!(actions.iter().any(|action| matches!(
            &action.1,
            ActionTarget::Downtimes(names) if names.len() == 2
        )));
        let acknowledgements = actions
            .iter()
            .find(|action| action.2 == Action::RemoveAcknowledgement)
            .expect("the acknowledgements");
        let ActionTarget::Objects(objects) = &acknowledgements.1 else {
            panic!("{:?}", acknowledgements.1);
        };
        assert_eq!(objects.len(), 2);
    });
}

#[test]
fn acknowledgements_from_a_dashboard_ask_with_the_same_listing() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let objects = vec![
            ObjectKey::host("sw-core-ams-02"),
            ObjectKey::service("web-edge-02", "http-tls"),
        ];
        // From a dashboard, several acknowledgements ask with the same
        // listing; nothing goes before Enter.
        request(
            app,
            cx,
            ObjectAction::RemoveAcknowledgement,
            objects.clone(),
        );
        assert_eq!(
            modal(app, cx),
            Some(ModalKind::Removal(RemovalKind::Acknowledgements))
        );
        let removal = removal_dialog(app, cx).read(cx).removal().clone();
        assert_eq!(removal.objects(), objects);
        assert_eq!(removal.button(), "remove 2 acknowledgements");
        app.keys(cx, "escape");
        assert!(recorder.actions().is_empty());
    });
}

#[test]
fn permissions_disable_removal_and_say_what_cant_be_read() {
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
        let downtimes = open(app, cx, ListKind::Downtimes);
        let redis = ObjectKey::service("cache-02", "redis-memory");
        let at = entry_at(
            &lines(&downtimes, cx),
            &EntryKey::Downtime(downtime_name(app, cx, &redis)),
        )
        .unwrap();
        app.click(cx, line_position(&downtimes, cx, at), Modifiers::default());
        app.keys(cx, "escape x backspace");
        assert_eq!(modal(app, cx), None, "no dialog for what can't be sent");
        let (_, title, detail) = toasts(app, cx).pop().unwrap();
        assert_eq!(title, "Can't remove downtimes");
        assert!(
            detail[0].starts_with("1 downtime: ") && detail[0].contains("viewer"),
            "{}",
            detail[0]
        );
        assert!(recorder.actions().is_empty());
        assert_eq!(downtimes.read(cx).marked().len(), 1);

        // Without objects/query/Comment and Downtime the downtimes view
        // says why instead of showing nothing as if there were none, and
        // handling says what it can't show.
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
                .list_denial(ListKind::Downtimes)
                .is_some_and(|denial| denial.contains("objects/query/Downtime"))
        );
        assert_eq!(state.list_denial(ListKind::Handling), None);
        let gap = state.handling_gap();
        assert!(
            gap.as_deref()
                .is_some_and(|gap| gap.contains("objects/query/Comment")),
            "{gap:?}"
        );
        // The acknowledgements still show.
        let handling = open(app, cx, ListKind::Handling);
        assert!(
            lines(&handling, cx)
                .iter()
                .any(|line| matches!(line, Line::Band { object, .. }
                    if *object == ObjectKey::host("sw-core-ams-02")))
        );
    });
}

#[test]
fn only_mine_hides_what_others_set() {
    run(FixtureOptions::default(), |app, cx| {
        let downtimes = open(app, cx, ListKind::Downtimes);
        assert_eq!(app.state.read(cx).author(), "icygui");
        downtimes.update(cx, |view, cx| {
            view.edit_options(cx, |options| options.only_mine = true);
        });
        app.draw(cx);
        assert_eq!(entries(&lines(&downtimes, cx)), 0);
        assert_eq!(downtimes.read(cx).summary().unwrap().by_others, 2);

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
        assert_eq!(entries(&lines(&downtimes, cx)), 1, "redis's");
        assert_eq!(downtimes.read(cx).summary().unwrap().by_others, 1);
    });
}

#[test]
fn thousands_of_downtimes_build_only_the_lines_on_screen() {
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
        let downtimes = open(app, cx, ListKind::Downtimes);
        downtimes.update(cx, |view, cx| {
            view.edit_options(cx, |options| options.pick_mode(Mode::List));
        });
        app.draw(cx);
        // Seven, then `+ 2994 more`; a click on it shows them all.
        let shown = lines(&downtimes, cx);
        let more = shown.len() - 1;
        assert!(
            matches!(shown[more], Line::More { hidden: 2_994, .. }),
            "{shown:#?}"
        );
        app.click(
            cx,
            line_position(&downtimes, cx, more),
            Modifiers::default(),
        );
        let shown = lines(&downtimes, cx);
        assert_eq!(entries(&shown), 3_002);
        let visible = downtimes.read(cx).visible_lines();
        assert!(visible.len() < 40, "{visible:?}");
        app.keys(cx, "end");
        let last = shown.len() - 1;
        assert_eq!(downtimes.read(cx).cursor(), Some(last));
        let visible = downtimes.read(cx).visible_lines();
        assert!(visible.contains(&last) && visible.len() < 40, "{visible:?}");

        // A ctrl-click marks an entry.
        app.keys(cx, "home");
        app.click(cx, line_position(&downtimes, cx, 3), secondary());
        assert_eq!(downtimes.read(cx).marked().len(), 1);

        // The selection bar's buttons stay put as the count grows (its
        // slot fits `999 selected`, as drawn).
        let buttons_x = |cx: &App| downtimes.read(cx).selection_buttons_x.get().unwrap();
        let one = buttons_x(cx);
        for _ in 0..9 {
            app.keys(cx, "shift-j");
        }
        assert_eq!(downtimes.read(cx).marked().len(), 10);
        assert_eq!(buttons_x(cx), one, "the buttons didn't move");
        for _ in 0..90 {
            app.keys(cx, "shift-j");
        }
        assert_eq!(downtimes.read(cx).marked().len(), 100);
        assert_eq!(buttons_x(cx), one, "the buttons didn't move");
        app.keys(cx, "secondary-a");
        assert_eq!(downtimes.read(cx).marked().len(), 3_002);
    });
}

#[test]
fn a_views_choices_go_by_keyboard_and_are_kept() {
    run(FixtureOptions::default(), |app, cx| {
        let downtimes = open(app, cx, ListKind::Downtimes);
        app.click(cx, line_position(&downtimes, cx, 1), Modifiers::default());
        app.keys(cx, "escape");
        let options = |view: &Entity<RecordList>, cx: &App| view.read(cx).current_options(cx);
        assert_eq!(options(&downtimes, cx).mode, Mode::Timeline, "by default");
        assert_eq!(
            options(&downtimes, cx).sort(ListKind::Downtimes),
            SortChoice::ByTime
        );
        // `m` only mine, `s` the next sort.
        app.keys(cx, "m s");
        assert!(options(&downtimes, cx).only_mine);
        let sorted = options(&downtimes, cx).sort(ListKind::Downtimes);
        assert_ne!(sorted, SortChoice::ByTime);
        // The list instead of the timeline.
        downtimes.update(cx, |view, cx| {
            view.edit_options(cx, |options| options.mode = Mode::List);
        });
        // Kept with the environment's UI state: closed and opened again,
        // the view comes back as it was.
        let saved = app.state.read(cx).list_options(ListKind::Downtimes);
        assert!(saved.only_mine && saved.sort.is_some(), "{saved:?}");
        assert_eq!(saved.mode.as_deref(), Some("list"));
        app.state.update(cx, |state, cx| {
            state.show_dashboard();
            cx.notify();
        });
        app.draw(cx);
        let downtimes = open(app, cx, ListKind::Downtimes);
        assert!(options(&downtimes, cx).only_mine);
        assert_eq!(options(&downtimes, cx).sort(ListKind::Downtimes), sorted);
        assert_eq!(options(&downtimes, cx).mode, Mode::List);
        let ui = app.state.read(cx).ui_state().clone();
        let environment = app
            .state
            .read(cx)
            .active_environment_id()
            .unwrap()
            .to_owned();
        assert_eq!(
            ui.environment(&environment).list_options["downtimes"],
            saved
        );

        // Handling keeps its chip.
        let handling = open(app, cx, ListKind::Handling);
        handling.update(cx, |view, cx| {
            view.edit_options(cx, |options| options.pick_chip(Chip::Upcoming));
        });
        let saved = app.state.read(cx).list_options(ListKind::Handling);
        assert_eq!(saved.chip.as_deref(), Some("upcoming"));
    });
}

/// The chips and the timeline go by keyboard too (`f`, `shift-f`, `v`),
/// kept like a click; `⌫` on a band with nothing marked says what to do.
#[test]
fn chips_and_the_timeline_go_by_keyboard() {
    run(FixtureOptions::default(), |app, cx| {
        let options = |view: &Entity<RecordList>, cx: &App| view.read(cx).current_options(cx);
        let downtimes = open(app, cx, ListKind::Downtimes);
        app.click(cx, line_position(&downtimes, cx, 1), Modifiers::default());
        app.keys(cx, "escape");
        app.keys(cx, "v");
        assert_eq!(options(&downtimes, cx).mode, Mode::List);
        let saved = app.state.read(cx).list_options(ListKind::Downtimes);
        assert_eq!(saved.mode.as_deref(), Some("list"));
        app.keys(cx, "v");
        assert_eq!(options(&downtimes, cx).mode, Mode::Timeline);
        app.keys(cx, "f");
        assert_eq!(options(&downtimes, cx).chip, Chip::InEffect);
        app.keys(cx, "shift-f shift-f");
        assert_eq!(
            options(&downtimes, cx).chip,
            Chip::FromConfig,
            "round the start"
        );
        let saved = app.state.read(cx).list_options(ListKind::Downtimes);
        assert_eq!(saved.chip.as_deref(), Some("from-config"));

        let handling = open(app, cx, ListKind::Handling);
        app.click(cx, line_position(&handling, cx, 1), Modifiers::default());
        app.keys(cx, "escape");
        app.keys(cx, "f v");
        assert_eq!(options(&handling, cx).chip, Chip::Acknowledged);
        assert_eq!(
            options(&handling, cx).mode,
            Mode::Timeline,
            "handling is a list"
        );

        // `⌫` on a band, nothing marked: no dialog, a word on what to do.
        app.keys(cx, "shift-f");
        let lines = lines(&handling, cx);
        let band = lines
            .iter()
            .position(|line| matches!(line, Line::Band { .. }))
            .expect("a band");
        app.click(cx, line_position(&handling, cx, band), Modifiers::default());
        app.keys(cx, "escape backspace");
        assert_eq!(modal(app, cx), None);
        let (_, title, _) = toasts(app, cx).pop().expect("a message");
        assert!(title.starts_with("Nothing to remove here"), "{title}");
    });
}

#[test]
fn the_pane_shows_the_thread_and_adds_a_comment() {
    run(FixtureOptions::default(), |app, cx| {
        let recorder = record(app, cx);
        let handling = open(app, cx, ListKind::Handling);
        let band = band_of(&lines(&handling, cx), &replication()).unwrap();
        app.click(
            cx,
            line_position(&handling, cx, band + 1),
            Modifiers::default(),
        );
        let pane = handling.read(cx).pane().expect("the pane");
        assert_eq!(pane.read(cx).object(), &replication());
        let field = pane.read(cx).comment_input().expect("the comment field");

        // `c` puts the keyboard in the field (the pane's, while it is open;
        // the thread's own otherwise: `ui_tests::comments`); Escape gives
        // it back.
        app.keys(cx, "c");
        let focused = |app: &Harness, cx: &mut App| {
            app.in_window(cx, |window, cx| {
                gpui::Focusable::focus_handle(field.read(cx), cx).is_focused(window)
            })
        };
        assert!(focused(app, cx));
        app.keys(cx, "escape");
        assert!(!focused(app, cx));
        assert_eq!(
            handling.read(cx).pane_object(cx),
            Some(replication()),
            "the pane stays"
        );

        // Typed and sent with Enter: one comment, through the action
        // path, by the environment's author. The list's and the pane's
        // letters are the field's while it has the keyboard.
        app.keys(cx, "c d a r k");
        assert_eq!(field.read(cx).value(cx), "dark");
        assert_eq!(modal(app, cx), None, "no dialog from the letters");
        app.keys(cx, "j x m s enter");
        assert_eq!(field.read(cx).value(cx), "", "cleared once sent");
        assert!(!focused(app, cx), "the keyboard back on the view");
        let actions = recorder.actions();
        assert_eq!(actions.len(), 1, "{actions:?}");
        assert_eq!(actions[0].1, ActionTarget::Objects(vec![replication()]));
        assert_eq!(
            actions[0].2,
            Action::AddComment {
                text: "darkjxms".to_owned(),
                expiry: None
            }
        );
    });
}
