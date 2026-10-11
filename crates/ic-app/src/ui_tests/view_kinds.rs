//! Topic 14, round 5 on the real window: handling and downtimes as view
//! kinds, stacked under their headers and as a page of their own; a list's
//! counts as state chips and rows at a view's own density; the sidebar's
//! marks; and the editor's new dashboard, sidebar mark and *copy filter
//! from…*.

use std::rc::Rc;

use gpui::{App, Modifiers, point, px};
use ic_config::{
    DowntimesMode, RowDensity, SidebarMark, StateChip, ThreadChip, ThreadOptions, View, ViewDisplay,
};
use ic_core::snapshot::DashboardRow;
use ic_model::{CheckableState, ObjectKey, ServiceState, Timestamp};
use ic_rules::{DashboardRef, ScopeSetting};
use ic_ui_kit::IconName;

use super::{Harness, run};
use crate::app_state::editing::DashboardDraft;
use crate::cluster::{ClusterEntry, ClusterState};
use crate::dashboard::page::{Densities, ItemKind, Page, Stop};
use crate::fixture::FixtureOptions;
use crate::lists::model::Chip;
use crate::sidebar::model::{Mark, cluster_rows, mark_and_count};

fn production() -> DashboardRef {
    super::production()
}

/// Gives `production` these views and mark, shows it, draws.
fn production_as(app: &Harness, cx: &mut App, views: Vec<View>, mark: SidebarMark) {
    app.state.update(cx, |state, cx| {
        state
            .update_dashboard(
                &production(),
                DashboardDraft {
                    name: "production".to_owned(),
                    views,
                    notifications: ScopeSetting::Inherit,
                    group_id: "demo-overview".to_owned(),
                    mark,
                },
            )
            .unwrap();
        state.select(production());
        cx.notify();
    });
    app.draw(cx);
}

fn page(app: &Harness, cx: &App) -> Rc<Page> {
    app.dashboard(cx).read(cx).page(cx).expect("a page")
}

fn handling() -> View {
    View {
        id: "handling".to_owned(),
        name: "handling".to_owned(),
        display: ViewDisplay::Handling,
        ..View::default()
    }
}

#[test]
fn handling_and_downtimes_stack_under_their_headers() {
    run(FixtureOptions::default(), |app, cx| {
        production_as(
            app,
            cx,
            vec![
                View {
                    id: "problems".to_owned(),
                    name: "problems".to_owned(),
                    density: Some(RowDensity::Compact),
                    ..View::default()
                },
                handling(),
                View {
                    id: "downtimes".to_owned(),
                    name: "downtimes".to_owned(),
                    display: ViewDisplay::Downtimes,
                    threads: ThreadOptions {
                        mode: DowntimesMode::List,
                        ..ThreadOptions::default()
                    },
                    ..View::default()
                },
            ],
            SidebarMark::Auto,
        );
        let page = page(app, cx);
        assert_eq!(page.views.len(), 3);
        // Handling's lines are the page's items, with stops of their own.
        assert!(page.views[1].thread.is_some());
        assert!(
            page.items
                .iter()
                .any(|item| item.view == 1 && matches!(item.kind, ItemKind::Thread { .. })),
            "handling's lines are on the page"
        );
        assert!(
            page.stops().iter().any(
                |entry| matches!(&entry.stop, Stop::Thread { view, .. } if &**view == "handling")
            ),
            "the cursor reaches them"
        );
        // The problems' rows at their own density.
        let compact = Densities::of(ic_ui_kit::ActiveTheme::theme(cx)).compact.row;
        let row = (0..page.items.len())
            .find(|&index| {
                page.items[index].view == 0
                    && matches!(page.items[index].kind, ItemKind::Row { .. })
            })
            .expect("a row");
        assert_eq!(page.item_height(row), compact, "compact rows on this view");
        assert_eq!(page.views[0].density, RowDensity::Compact);

        // A chip picked in the stacked header is kept with the view.
        app.state.update(cx, |state, cx| {
            state.update_view(&production(), "handling", |view| {
                view.threads.chip = ThreadChip::Comments;
            });
            cx.notify();
        });
        app.draw(cx);
        let page = super::views_page(app, cx);
        let thread = page.views[1].thread.as_ref().unwrap();
        assert_eq!(thread.options.chip, Chip::Comments);
        // Neither view counts toward the sidebar's number.
        let state = app.state.read(cx);
        let (_, dashboard) = state.dashboard(&production()).unwrap();
        assert!(dashboard.has_problem_view());
        let (mark, _) = mark_and_count(
            dashboard,
            state.result(&production()),
            state.snapshot(),
            Timestamp::now(),
        );
        assert!(
            matches!(mark, Mark::Dot(_)),
            "a problem view: the state's dot"
        );
    });
}

#[test]
fn a_dashboard_of_one_handling_view_is_that_views_page() {
    run(FixtureOptions::default(), |app, cx| {
        production_as(app, cx, vec![handling()], SidebarMark::Auto);
        let list = app
            .workspace
            .read(cx)
            .dashboard_list(&production())
            .cloned()
            .expect("handling's own page");
        assert!(!list.read(cx).lines().is_empty(), "its threads");
        let state = app.state.read(cx);
        let (_, dashboard) = state.dashboard(&production()).unwrap();
        let (mark, count) = mark_and_count(
            dashboard,
            state.result(&production()),
            state.snapshot(),
            Timestamp::now(),
        );
        assert_eq!(mark, Mark::Icon(IconName::Users), "the kind's icon");
        assert!(count.is_some(), "the objects being handled");

        // A chosen icon replaces it; the count stays the view's.
        production_as(
            app,
            cx,
            vec![handling()],
            SidebarMark::Icon("phone".to_owned()),
        );
        let state = app.state.read(cx);
        let (_, dashboard) = state.dashboard(&production()).unwrap();
        let (mark, again) = mark_and_count(
            dashboard,
            state.result(&production()),
            state.snapshot(),
            Timestamp::now(),
        );
        assert_eq!(mark, Mark::Icon(IconName::Phone));
        assert_eq!(again, count);

        // The view turned into downtimes: the page is downtimes' now.
        production_as(
            app,
            cx,
            vec![View {
                display: ViewDisplay::Downtimes,
                ..handling()
            }],
            SidebarMark::Auto,
        );
        let list = app
            .workspace
            .read(cx)
            .dashboard_list(&production())
            .cloned()
            .expect("downtimes' own page");
        assert_eq!(list.read(cx).kind(), crate::lists::ListKind::Downtimes);
    });
}

#[test]
fn a_lists_counts_are_its_state_chips() {
    run(FixtureOptions::default(), |app, cx| {
        production_as(
            app,
            cx,
            vec![View {
                id: "problems".to_owned(),
                ..View::default()
            }],
            SidebarMark::Auto,
        );
        let before = app
            .state
            .read(cx)
            .view_result(&production(), "problems")
            .unwrap()
            .clone();
        assert!(before.counts.critical > 0 && before.counts.warning > 0);
        app.state.update(cx, |state, cx| {
            state.update_view(&production(), "problems", |view| {
                view.state = Some(StateChip::Critical);
            });
            cx.notify();
        });
        app.draw(cx);
        let state = app.state.read(cx);
        let after = state.view_result(&production(), "problems").unwrap();
        assert!(!after.rows().is_empty());
        for row in after.rows() {
            let DashboardRow::Object(ObjectKey::Service { key }) = row else {
                panic!("a service row: {row:?}");
            };
            let service = state.snapshot().services.get(key).unwrap();
            assert_eq!(
                CheckableState::Service(service.state),
                CheckableState::Service(ServiceState::Critical)
            );
        }
        assert_eq!(after.counts, before.counts, "the counts stay, every state");
    });
}

#[test]
fn a_new_dashboard_starts_empty_and_takes_any_kind() {
    run(FixtureOptions::default(), |app, cx| {
        app.keys(cx, "ctrl-n");
        let editor = app.workspace.read(cx).editor().unwrap().clone();
        let draft = editor.read(cx).draft().clone();
        assert_eq!(draft.views.len(), 1);
        assert_eq!(draft.views[0].name, "list");
        assert!(draft.views[0].filter.is_empty(), "no starting point");

        // *copy filter from…*: another dashboard's filter fills the field,
        // and the field's undo takes it back.
        let source = editor
            .read(cx)
            .copy_sources_for_test("", cx)
            .into_iter()
            .next()
            .expect("a filter to copy");
        app.in_window(cx, |window, cx| {
            editor.update(cx, |editor, cx| {
                editor.copy_filter_for_test(&source.filter, window, cx);
            });
        });
        app.draw(cx);
        assert_eq!(editor.read(cx).draft().views[0].filter, source.filter);
        app.keys(cx, "ctrl-z");
        assert_eq!(
            editor.read(cx).draft().views[0].filter,
            "",
            "undone in the field"
        );

        // Another kind: handling previews as its own page.
        editor.update(cx, |editor, cx| {
            editor.change_selected_for_test(cx, |view| {
                *view = crate::editor::model::with_display(view.clone(), ViewDisplay::Handling);
            });
        });
        app.draw(cx);
        assert!(editor.read(cx).threads_preview().is_some());

        // An icon as the sidebar mark, kept with the dashboard and among
        // the recent icons.
        editor.update(cx, |editor, cx| {
            editor.pick_icon_for_test(IconName::Phone, cx);
        });
        app.keys(cx, "ctrl-s");
        let state = app.state.read(cx);
        let saved = state
            .selected()
            .cloned()
            .expect("the new dashboard is shown");
        let (_, dashboard) = state.dashboard(&saved).unwrap();
        assert_eq!(dashboard.mark, SidebarMark::Icon("phone".to_owned()));
        assert_eq!(dashboard.views[0].display, ViewDisplay::Handling);
        assert_eq!(
            state.recent_icons().first().map(String::as_str),
            Some("phone")
        );
    });
}

/// The middle of the cluster section's entry `index` in the sidebar: under
/// the 41px header and the section's 36px heading, 30px rows.
fn cluster_entry(index: usize) -> gpui::Point<gpui::Pixels> {
    #[expect(clippy::cast_precision_loss, reason = "four rows")]
    let row = 30. * index as f32;
    point(px(150.), px(41. + 36. + 15. + row))
}

#[test]
fn the_cluster_section_opens_its_pages_from_the_sidebar() {
    run(FixtureOptions::default(), |app, cx| {
        for (index, entry) in ClusterEntry::ALL.into_iter().enumerate() {
            app.click(cx, cluster_entry(index), Modifiers::default());
            assert_eq!(app.state.read(cx).active_cluster(), Some(entry));
            let shown = app.workspace.read(cx).shown_page();
            let expected = match entry {
                ClusterEntry::Handling | ClusterEntry::Downtimes => "list",
                ClusterEntry::Events => "events",
                ClusterEntry::Health => "health",
            };
            assert_eq!(shown, expected, "{entry:?}");
            let state = app.state.read(cx);
            let rows = cluster_rows(
                state.snapshot(),
                state.active_cluster(),
                ClusterState::Ok,
                Timestamp::now(),
            );
            let active: Vec<_> = rows.iter().filter(|row| row.active).collect();
            assert_eq!(active.len(), 1);
            assert_eq!(active[0].entry, entry, "highlighted");
        }
        // Handling and downtimes count the whole environment; events and
        // health have no number.
        let state = app.state.read(cx);
        let rows = cluster_rows(state.snapshot(), None, ClusterState::Ok, Timestamp::now());
        assert!(rows[0].count.is_some_and(|count| count > 0), "{rows:?}");
        assert!(rows[1].count.is_some_and(|count| count > 0), "{rows:?}");
        assert_eq!(rows[2].count, None);
        assert_eq!(rows[3].count, None);
        assert_eq!(rows[3].mark, Mark::Dot(crate::sidebar::Dot::Ok));

        // A dashboard shows again from its row; the section stays.
        app.click(cx, super::sidebar_item(0), Modifiers::default());
        assert_eq!(app.state.read(cx).active_cluster(), None);
        assert_eq!(app.workspace.read(cx).shown_page(), "dashboard");
    });
}

#[test]
fn dashboards_without_problem_views_show_their_first_views_kind() {
    run(FixtureOptions::default(), |app, cx| {
        let cluster = {
            let state = app.state.read(cx);
            cluster_rows(state.snapshot(), None, ClusterState::Ok, Timestamp::now())
        };
        let one = |id: &str, display: ViewDisplay| View {
            id: id.to_owned(),
            name: id.to_owned(),
            display,
            ..View::default()
        };
        let cases = [
            (ViewDisplay::Handling, IconName::Users, cluster[0].count),
            (
                ViewDisplay::Downtimes,
                IconName::CalendarClock,
                cluster[1].count,
            ),
            (ViewDisplay::EventStream, IconName::Activity, None),
        ];
        for (display, icon, count) in cases {
            production_as(app, cx, vec![one("only", display)], SidebarMark::Auto);
            let state = app.state.read(cx);
            let (_, dashboard) = state.dashboard(&production()).unwrap();
            assert!(!dashboard.has_problem_view());
            let (mark, shown) = mark_and_count(
                dashboard,
                state.result(&production()),
                state.snapshot(),
                Timestamp::now(),
            );
            assert_eq!(mark, Mark::Icon(icon), "{display:?}");
            // Without a filter, the view counts what the cluster's does.
            assert_eq!(
                shown.map(|count| count as usize),
                count,
                "{display:?}: the view's own count"
            );
        }

        // Behind a problem view, the state's dot and the problem count.
        production_as(
            app,
            cx,
            vec![
                one("handling", ViewDisplay::Handling),
                one("problems", ViewDisplay::List),
            ],
            SidebarMark::Auto,
        );
        let state = app.state.read(cx);
        let (_, dashboard) = state.dashboard(&production()).unwrap();
        let (mark, count) = mark_and_count(
            dashboard,
            state.result(&production()),
            state.snapshot(),
            Timestamp::now(),
        );
        assert!(matches!(mark, Mark::Dot(_)), "{mark:?}");
        assert_eq!(
            count,
            state
                .result(&production())
                .map(|result| result.summary.unhandled)
        );
    });
}
