//! Topic 04, 05 and the host-with-services style on the real window: a
//! host's band with its two click targets, paging by count, folding with
//! the keyboard, a page of several views that one cursor moves through, a
//! host-group grid, and the handled slot.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Modifiers, Pixels, Point, point, px};
use ic_config::{GroupBy, View, ViewDisplay};
use ic_core::snapshot::{Snapshot, ViewBody};
use ic_model::{ObjectKey, Timestamp};
use ic_rules::{DashboardRef, ScopeSetting};

use super::{Harness, run};
use crate::app_state::editing::DashboardDraft;
use crate::dashboard::page::{ItemKind, Page, Stop};
use crate::fixture::FixtureOptions;

fn databases() -> DashboardRef {
    DashboardRef {
        group_id: "demo-overview".to_owned(),
        dashboard_id: "demo-overview-databases".to_owned(),
    }
}

fn production() -> DashboardRef {
    super::production()
}

fn overview() -> DashboardRef {
    DashboardRef {
        group_id: "demo-overview".to_owned(),
        dashboard_id: "demo-overview-overview".to_owned(),
    }
}

/// The overview's only view.
const OVERVIEW_VIEW: &str = "demo-overview-overview-view";

/// Shows `reference`, draws.
fn show(app: &Harness, cx: &mut App, reference: &DashboardRef) {
    app.state.update(cx, |state, cx| {
        state.select(reference.clone());
        cx.notify();
    });
    app.draw(cx);
}

/// The page as built for the last frame.
fn page(app: &Harness, cx: &App) -> Rc<Page> {
    app.dashboard(cx).read(cx).page(cx).expect("a page")
}

/// The index of the first item that `matches`.
fn item(page: &Page, matches: impl Fn(&Page, usize) -> bool) -> usize {
    (0..page.items.len())
        .find(|&index| matches(page, index))
        .expect("an item")
}

/// The band of host `host` in the page's first view with groups.
fn band_of(page: &Page, host: &str) -> usize {
    item(page, |page, index| {
        let entry = page.items[index];
        match entry.kind {
            ItemKind::Band { group } => page.views[entry.view].groups[group]
                .host
                .as_ref()
                .is_some_and(|name| name.as_str() == host),
            _ => false,
        }
    })
}

/// Where item `index` is drawn.
fn bounds(app: &Harness, cx: &App, index: usize) -> gpui::Bounds<Pixels> {
    app.dashboard(cx)
        .read(cx)
        .item_bounds(index, cx)
        .expect("drawn")
}

/// The chevron of a band or view header (14px in, 12px wide).
fn chevron(bounds: gpui::Bounds<Pixels>) -> Point<Pixels> {
    point(bounds.left() + px(20.), bounds.center().y)
}

/// A band's name: past the chevron and the 44px mark column.
fn band_name(bounds: gpui::Bounds<Pixels>) -> Point<Pixels> {
    point(bounds.left() + px(18. + 44. + 14. + 24.), bounds.center().y)
}

fn group_of<'a>(page: &'a Page, host: &str) -> &'a crate::dashboard::page::ListGroup {
    page.views
        .iter()
        .flat_map(|view| &view.groups)
        .find(|group| {
            group
                .host
                .as_ref()
                .is_some_and(|name| name.as_str() == host)
        })
        .expect("the host's group")
}

/// Shows the databases dashboard with every service of its hosts (not
/// only problems), so its hosts page.
fn databases_with_every_service(app: &Harness, cx: &mut App) {
    show(app, cx, &databases());
    app.state.update(cx, |state, cx| {
        assert!(state.update_primary_view(&databases(), |view| {
            view.problems_only = false;
        }));
        cx.notify();
    });
    app.draw(cx);
}

#[test]
fn a_hosts_band_has_two_click_targets() {
    run(FixtureOptions::default(), |app, cx| {
        databases_with_every_service(app, cx);
        let page = page(app, cx);
        let band = band_of(&page, "db-prod-03");
        let host = ObjectKey::host("db-prod-03");

        // The chevron only collapses (and expands): no pane opens.
        app.click(cx, chevron(bounds(app, cx, band)), Modifiers::default());
        assert!(group_of(&page_now(app, cx), "db-prod-03").collapsed);
        assert_eq!(app.pane_object(cx), None, "the chevron opens nothing");
        let band = band_of(&page_now(app, cx), "db-prod-03");
        app.click(cx, chevron(bounds(app, cx, band)), Modifiers::default());
        assert!(!group_of(&page_now(app, cx), "db-prod-03").collapsed);
        assert_eq!(app.pane_object(cx), None);

        // Anywhere else on the band opens the host in the pane, and folds
        // nothing.
        let band = band_of(&page_now(app, cx), "db-prod-03");
        app.click(cx, band_name(bounds(app, cx, band)), Modifiers::default());
        assert_eq!(app.pane_object(cx), Some(host.clone()));
        assert!(!group_of(&page_now(app, cx), "db-prod-03").collapsed);
        // The counts on the right open it too.
        app.keys(cx, "escape");
        let band = band_of(&page_now(app, cx), "db-prod-03");
        let right = bounds(app, cx, band);
        app.click(
            cx,
            point(right.right() - px(30.), right.center().y),
            Modifiers::default(),
        );
        assert_eq!(app.pane_object(cx), Some(host));
        // Enter on the band does what a click does.
        app.keys(cx, "escape enter");
        assert_eq!(app.pane_object(cx), Some(ObjectKey::host("db-prod-03")));
    });
}

/// The page as built for the last frame (after the last action).
fn page_now(app: &Harness, cx: &App) -> Rc<Page> {
    page(app, cx)
}

#[test]
fn hosts_page_by_count_and_fold_with_the_keyboard() {
    run(FixtureOptions::default(), |app, cx| {
        databases_with_every_service(app, cx);
        let page = page(app, cx);
        let group = group_of(&page, "db-prod-03");
        let total = group.members.len();
        assert!(total > 7, "db-prod-03 has {total} services");
        assert_eq!(group.shown, 7, "up to 7 rows");
        assert!(group.pages);
        // Its problems come first.
        let first = group.members[0];
        let view = &page.views[0];
        assert!(matches!(
            &view.rows[first],
            ic_core::snapshot::DashboardRow::Object(key) if *key == super::replication()
        ));

        // `+ N more` shows the whole host in place.
        let more = item(
            &page,
            |page, index| matches!(page.items[index].kind, ItemKind::More { group } if page.views[0].groups[group].host.as_ref().is_some_and(|host| host.as_str() == "db-prod-03")),
        );
        app.click(cx, bounds(app, cx, more).center(), Modifiers::default());
        let group = group_of(&page_now(app, cx), "db-prod-03").clone();
        assert_eq!(
            (group.shown, group.hidden(), group.expanded),
            (total, 0, true)
        );
        assert_eq!(app.pane_object(cx), None, "nothing opens");

        // The keyboard: the cursor on the band, ← collapses, → expands.
        let band = band_of(&page_now(app, cx), "db-prod-03");
        app.click(cx, band_name(bounds(app, cx, band)), Modifiers::default());
        app.keys(cx, "escape");
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::Band { .. })
        ));
        app.keys(cx, "left");
        assert!(group_of(&page_now(app, cx), "db-prod-03").collapsed);
        app.keys(cx, "right");
        assert!(!group_of(&page_now(app, cx), "db-prod-03").collapsed);

        // Down to the paging row (now `− show fewer`): ← pages again, the
        // cursor staying on it; → shows all, the cursor on the first row
        // that appeared, where the paging row was.
        let steps = vec!["j"; total + 1].join(" ");
        app.keys(cx, &steps);
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::More { .. })
        ));
        app.keys(cx, "left");
        let group = group_of(&page_now(app, cx), "db-prod-03").clone();
        assert_eq!((group.shown, group.expanded), (7, false));
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::More { .. })
        ));
        app.keys(cx, "right");
        assert!(group_of(&page_now(app, cx), "db-prod-03").expanded);
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::Row { .. })
        ));
        // Enter on the paging row (past the rest of the rows) pages again.
        app.keys(cx, &vec!["j"; total - 7].join(" "));
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::More { .. })
        ));
        app.keys(cx, "enter");
        assert!(!group_of(&page_now(app, cx), "db-prod-03").expanded);
    });
}

#[test]
fn folding_hides_rows_but_not_their_marks() {
    run(FixtureOptions::default(), |app, cx| {
        databases_with_every_service(app, cx);
        let page = page(app, cx);
        let total = page.views[0].objects().len();
        // ctrl-a marks every row of the view, those paged away too.
        app.keys(cx, "j ctrl-a");
        assert_eq!(app.marked(cx).len(), total);
        // Collapsing a host keeps its marks; Escape clears them all.
        let band = band_of(&page_now(app, cx), "db-prod-03");
        app.click(cx, chevron(bounds(app, cx, band)), Modifiers::default());
        assert!(group_of(&page_now(app, cx), "db-prod-03").collapsed);
        assert_eq!(
            app.marked(cx).len(),
            total,
            "collapsing only changes what shows"
        );
        // The action keys take every marked object.
        app.keys(cx, "r");
        let request = app.state.read(cx).last_request().cloned().unwrap();
        assert_eq!(request.targets.len(), total);
        app.keys(cx, "escape escape");
        assert!(app.marked(cx).is_empty());
    });
}

/// Gives `production` several views: a grid of its hosts, its problems,
/// a grouped list by host and an empty list.
fn production_with_views(app: &Harness, cx: &mut App) -> Vec<String> {
    let views = vec![
        View {
            id: "grid".to_owned(),
            name: "hosts by group".to_owned(),
            display: ViewDisplay::HostGroupGrid,
            object_kind: ic_config::ObjectKind::Hosts,
            ..View::default()
        },
        View {
            id: "problems".to_owned(),
            name: "service problems".to_owned(),
            filter: "host.vars.env == \"prod\"".to_owned(),
            ..View::default()
        },
        View {
            id: "nothing".to_owned(),
            name: "nothing here".to_owned(),
            filter: "service.name == \"no-such-service\"".to_owned(),
            ..View::default()
        },
        {
            let mut grouped = View {
                id: "grouped".to_owned(),
                name: "by host".to_owned(),
                filter: "host.vars.role == \"postgres\"".to_owned(),
                ..View::default()
            };
            grouped.set_grouping(GroupBy::Host);
            grouped
        },
    ];
    let ids = views.iter().map(|view| view.id.clone()).collect();
    app.state.update(cx, |state, cx| {
        state
            .update_dashboard(
                &production(),
                DashboardDraft {
                    name: "production".to_owned(),
                    views,
                    notifications: ScopeSetting::Inherit,
                    group_id: "demo-overview".to_owned(),
                },
            )
            .unwrap();
        state.select(production());
        cx.notify();
    });
    app.draw(cx);
    ids
}

fn header_of(page: &Page, view: usize) -> usize {
    item(page, |page, index| {
        page.items[index].view == view && page.items[index].kind == ItemKind::Header
    })
}

#[test]
fn one_cursor_moves_through_every_view() {
    run(FixtureOptions::default(), |app, cx| {
        production_with_views(app, cx);
        let page = page(app, cx);
        assert_eq!(page.views.len(), 4);
        assert!(
            page.items
                .iter()
                .filter(|item| item.kind == ItemKind::Header)
                .count()
                == 4,
            "every view has its header"
        );
        assert!(
            page.views[2].rows.is_empty(),
            "an empty view: its header only"
        );
        assert!(
            !page
                .items
                .iter()
                .any(|item| item.view == 2 && item.kind != ItemKind::Header),
            "no body, no empty box"
        );

        // j starts on the grid's first host; Tab jumps to the next view's
        // first row, skipping the empty one; shift-Tab back.
        app.keys(cx, "j");
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::Cell { .. })
        ));
        app.keys(cx, "tab");
        let stop = app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        assert!(
            matches!(&stop, Stop::Row { view, .. } if &**view == "problems"),
            "{stop:?}"
        );
        app.keys(cx, "tab");
        let stop = app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        assert!(
            matches!(&stop, Stop::Header(view) if &**view == "nothing"),
            "a view without rows: its header ({stop:?})"
        );
        app.keys(cx, "tab");
        let stop = app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        assert!(
            matches!(&stop, Stop::Band { view, .. } if &**view == "grouped"),
            "{stop:?}"
        );
        app.keys(cx, "shift-tab shift-tab");
        let stop = app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        assert!(matches!(&stop, Stop::Row { view, .. } if &**view == "problems"));

        // Enter opens the row's object; j goes on through the view.
        app.keys(cx, "enter");
        let first = app.pane_object(cx).unwrap();
        app.keys(cx, "j");
        assert_ne!(
            app.pane_object(cx),
            Some(first),
            "the pane follows the cursor"
        );

        // ctrl-a marks the focused view's rows only.
        app.keys(cx, "ctrl-a");
        assert_eq!(app.marked(cx), page_now(app, cx).views[1].objects());
        app.keys(cx, "escape escape");
        assert!(app.marked(cx).is_empty());
    });
}

#[test]
fn view_headers_collapse_with_the_chevron_and_the_arrows() {
    run(FixtureOptions::default(), |app, cx| {
        production_with_views(app, cx);
        let page = page(app, cx);
        let header = header_of(&page, 1);
        app.click(cx, chevron(bounds(app, cx, header)), Modifiers::default());
        let page = page_now(app, cx);
        assert!(page.views[1].collapsed);
        assert!(
            !page
                .items
                .iter()
                .any(|item| item.view == 1 && item.kind != ItemKind::Header),
            "a collapsed view keeps only its header"
        );
        assert_eq!(app.pane_object(cx), None);
        // j skips the collapsed view.
        app.keys(cx, "j");
        let grid_cells = page.views[0]
            .grid
            .as_ref()
            .unwrap()
            .grid
            .groups
            .iter()
            .map(|group| group.cells.len())
            .sum::<usize>();
        app.keys(cx, &vec!["right"; grid_cells].join(" "));
        app.keys(cx, &["j"; 8].join(" "));
        let stop = app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        assert!(stop.view().as_ref() != "problems", "{stop:?}");

        // Its header (a click on it) takes the cursor: → expands it, ←
        // collapses it again.
        let header = header_of(&page_now(app, cx), 1);
        let header_bounds = bounds(app, cx, header);
        app.click(
            cx,
            point(header_bounds.left() + px(200.), header_bounds.center().y),
            Modifiers::default(),
        );
        assert!(matches!(
            app.dashboard(cx).read(cx).cursor_stop(cx),
            Some(Stop::Header(_))
        ));
        app.keys(cx, "right");
        assert!(!page_now(app, cx).views[1].collapsed);
        app.keys(cx, "left");
        assert!(page_now(app, cx).views[1].collapsed);
    });
}

#[test]
fn every_view_folds_from_the_keyboard_alone() {
    run(FixtureOptions::default(), |app, cx| {
        production_with_views(app, cx);
        let cursor = |app: &Harness, cx: &App| app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        // j: the grid's first host; Tab: the problems list's first row.
        app.keys(cx, "j tab");
        let stop = cursor(app, cx);
        assert!(
            matches!(&stop, Stop::Row { view, .. } if &**view == "problems"),
            "{stop:?}"
        );
        // ← on a row: its view's header; ← again folds the view, → unfolds.
        app.keys(cx, "left");
        let stop = cursor(app, cx);
        assert!(
            matches!(&stop, Stop::Header(view) if &**view == "problems"),
            "{stop:?}"
        );
        app.keys(cx, "left");
        assert!(page_now(app, cx).views[1].collapsed);
        app.keys(cx, "right");
        assert!(!page_now(app, cx).views[1].collapsed);

        // A grid's first host: ← goes to the grid's header.
        app.keys(cx, "shift-tab");
        assert!(matches!(cursor(app, cx), Stop::Cell { .. }));
        app.keys(cx, "left left");
        assert!(page_now(app, cx).views[0].collapsed, "the grid folds");
        app.keys(cx, "right");

        // A grouped list: a row → its band (← folds it) → the header.
        app.keys(cx, "tab tab tab j");
        let stop = cursor(app, cx);
        assert!(
            matches!(&stop, Stop::Row { view, group: Some(_), .. } if &**view == "grouped"),
            "{stop:?}"
        );
        app.keys(cx, "left");
        assert!(matches!(cursor(app, cx), Stop::Band { .. }));
        app.keys(cx, "left");
        let page = page_now(app, cx);
        assert!(page.views[3].groups[0].collapsed, "the band folds");
        app.keys(cx, "left");
        let stop = cursor(app, cx);
        assert!(
            matches!(&stop, Stop::Header(view) if &**view == "grouped"),
            "{stop:?}"
        );
        app.keys(cx, "left");
        assert!(page_now(app, cx).views[3].collapsed);
    });
}

#[test]
fn under_a_group_filter_ctrl_a_marks_only_what_shows() {
    run(FixtureOptions::default(), |app, cx| {
        production_with_views(app, cx);
        let page = page(app, cx);
        let layout = page.views[0].grid.clone().unwrap();
        let group = layout.grid.groups[0].clone();
        let line = item(&page, |page, index| {
            matches!(page.items[index].kind, ItemKind::Grid { line: 0 })
        });
        let line_bounds = bounds(app, cx, line);
        // A click on the first group's name filters the page.
        app.click(
            cx,
            point(
                line_bounds.left() + px(18. + 8. + 9. + 10.),
                line_bounds.top() + px(14. + 9.),
            ),
            Modifiers::default(),
        );
        let filter = app
            .dashboard(cx)
            .read(cx)
            .group_filter(cx)
            .expect("filtered");
        assert_eq!(filter.name, group.name);
        // Tab: the cursor stays in the filtered group, then the list.
        app.keys(cx, "tab");
        let stop = app.dashboard(cx).read(cx).cursor_stop(cx).unwrap();
        assert!(
            matches!(&stop, Stop::Cell { group: in_group, .. } if **in_group == *group.name),
            "{stop:?}"
        );
        app.keys(cx, "tab ctrl-a");
        let snapshot: Arc<Snapshot> = app.state.read(cx).snapshot().clone();
        let marked = app.marked(cx);
        assert!(!marked.is_empty());
        assert!(
            marked
                .iter()
                .all(|key| filter.includes_object(&snapshot, key)),
            "nothing the filter hides is marked"
        );
        let rows = &page_now(app, cx).views[1];
        let shown = rows
            .rows
            .iter()
            .filter(|row| matches!(row, ic_core::snapshot::DashboardRow::Object(key) if filter.includes_object(&snapshot, key)))
            .count();
        assert_eq!(marked.len(), shown);
    });
}

#[test]
fn edit_view_opens_the_editor_on_that_view() {
    run(FixtureOptions::default(), |app, cx| {
        production_with_views(app, cx);
        app.dashboard(cx).update(cx, |_, cx| {
            cx.emit(crate::dashboard::DashboardEvent::EditView(
                production(),
                "grouped".to_owned(),
            ));
        });
        app.draw(cx);
        let editor = app.workspace.read(cx).editor().expect("the editor").clone();
        assert_eq!(editor.read(cx).selected(), "grouped");
    });
}

#[test]
fn a_grid_host_opens_its_pane_and_a_group_filters_the_page() {
    run(FixtureOptions::default(), |app, cx| {
        production_with_views(app, cx);
        let page = page(app, cx);
        let layout = page.views[0].grid.clone().unwrap();
        let first = &layout.grid.groups[0];
        let host = first.cells[0].host.clone();

        // A click on a square opens its host.
        let line = item(&page, |page, index| {
            matches!(page.items[index].kind, ItemKind::Grid { line: 0 })
        });
        let line_bounds = bounds(app, cx, line);
        let square = point(
            line_bounds.left() + px(18. + 6.),
            line_bounds.top() + px(14. + 18. + 8. + 6.),
        );
        app.click(cx, square, Modifiers::default());
        assert_eq!(
            app.pane_object(cx),
            Some(ObjectKey::Host { name: host.clone() })
        );
        // → moves to the next host, the pane follows.
        app.keys(cx, "right");
        assert_eq!(
            app.pane_object(cx),
            Some(ObjectKey::Host {
                name: first
                    .cells
                    .get(1)
                    .map_or(host.clone(), |cell| cell.host.clone())
            })
        );
        app.keys(cx, "escape");

        // A click on a group's name filters the page to it: the list shows
        // only that group's hosts' rows.
        let name = point(
            line_bounds.left() + px(18. + 8. + 9. + 10.),
            line_bounds.top() + px(14. + 9.),
        );
        app.click(cx, name, Modifiers::default());
        let filter = app
            .dashboard(cx)
            .read(cx)
            .group_filter(cx)
            .expect("filtered");
        assert_eq!(filter.name, first.name);
        let snapshot: Arc<Snapshot> = app.state.read(cx).snapshot().clone();
        let filtered = page_now(app, cx);
        for item in filtered.items.iter().filter(|item| item.view == 1) {
            if let ItemKind::Row { row, .. } = item.kind
                && let ic_core::snapshot::DashboardRow::Object(key) = &filtered.views[1].rows[row]
            {
                assert!(
                    filter.includes_object(&snapshot, key),
                    "{key} is in the group"
                );
            }
        }
        assert_eq!(
            filtered.views[0].grid.as_ref().unwrap().on,
            Some(0),
            "its group is ringed"
        );
        // Escape clears it.
        app.keys(cx, "escape");
        assert_eq!(app.dashboard(cx).read(cx).group_filter(cx), None);
    });
}

#[test]
fn the_handled_slot_shows_and_hides_without_changing_the_counts() {
    run(FixtureOptions::default(), |app, cx| {
        // The overview hides handled problems (the settings), and some are
        // handled. It is a single list: the slot is the summary bar's, at
        // its right end.
        show(app, cx, &overview());
        let hidden = app
            .state
            .read(cx)
            .view_result(&overview(), OVERVIEW_VIEW)
            .map(|result| result.hidden)
            .unwrap();
        assert!(hidden > 0, "something handled is hidden");
        let slot = point(px(1440. - 18. - 10.), px(41. + 18.));
        app.click(cx, slot, Modifiers::default());
        let result = app
            .state
            .read(cx)
            .view_result(&overview(), OVERVIEW_VIEW)
            .cloned()
            .unwrap();
        assert_eq!(result.hidden, 0, "shown, hollow");
        assert!(result.handled > 0);
        let counts_before = result.counts;
        app.click(cx, slot, Modifiers::default());
        let result = app
            .state
            .read(cx)
            .view_result(&overview(), OVERVIEW_VIEW)
            .cloned()
            .unwrap();
        assert_eq!(result.hidden, hidden, "hidden again");
        assert_eq!(
            result.counts, counts_before,
            "show and hide never change the counts"
        );
    });
}

#[test]
fn a_stream_lists_its_events_and_enter_opens_one() {
    run(FixtureOptions::default(), |app, cx| {
        let mut views = vec![View {
            id: "stream".to_owned(),
            name: "events".to_owned(),
            display: ViewDisplay::EventStream,
            ..View::default()
        }];
        views[0].stream.lines = 4;
        app.state.update(cx, |state, cx| {
            state
                .update_dashboard(
                    &production(),
                    DashboardDraft {
                        name: "production".to_owned(),
                        views,
                        notifications: ScopeSetting::Inherit,
                        group_id: "demo-overview".to_owned(),
                    },
                )
                .unwrap();
            cx.notify();
        });
        // The core fills streams from the event log; the fixture has none.
        let now = Timestamp::now();
        let events: Vec<ic_core::LogEntry> = (0..6)
            .map(|index| ic_core::LogEntry {
                at: Timestamp::from_unix_seconds(now.as_unix_seconds() - f64::from(index) * 60.),
                object: if index == 2 {
                    ObjectKey::host("db-prod-03")
                } else {
                    super::replication()
                },
                kind: ic_core::LogKind::CommentAdded,
                text: format!("note {index}"),
                author: Some("m.keller".to_owned()),
            })
            .collect();
        app.state.update(cx, |state, cx| {
            let old = state.snapshot().clone();
            let mut dashboards = (*old.dashboards).clone();
            let result = dashboards.get_mut(&production()).unwrap();
            result.views[0].body = ViewBody::Stream(Arc::new(events));
            state.set_snapshot(Arc::new(Snapshot {
                revision: old.revision + 1,
                dashboards: Arc::new(dashboards),
                ..(*old).clone()
            }));
            cx.notify();
        });
        app.draw(cx);
        let page = page(app, cx);
        assert_eq!(page.views[0].lines, 4, "4 lines show, the rest scroll");
        assert_eq!(page.item_height(1), page.sizes().event * 4.);
        // Tab into the stream, down twice: the host's event.
        app.keys(cx, "tab j j enter");
        assert_eq!(app.pane_object(cx), Some(ObjectKey::host("db-prod-03")));
    });
}

#[test]
fn twenty_thousand_rows_by_host_build_only_whats_on_screen() {
    run(
        FixtureOptions {
            generated_rows: 20_000,
        },
        |app, cx| {
            let load = app.state.read(cx).selected().unwrap().clone();
            app.state.update(cx, |state, cx| {
                assert!(state.update_primary_view(&load, |view| view.set_grouping(GroupBy::Host)));
                cx.notify();
            });
            let started = std::time::Instant::now();
            app.draw(cx);
            let first = started.elapsed();
            let page = page(app, cx);
            let groups = &page.views[0].groups;
            assert_eq!(groups.len(), 1_000, "a band per host");
            // Up to 7 rows a host (every problem), the rest paged.
            assert!(
                groups
                    .iter()
                    .all(|group| group.shown >= 7 && group.members.len() == 20)
            );
            assert!(groups.iter().any(|group| group.shown == 7 && group.pages));
            // A band, its rows and the paging row a host: far fewer items
            // than rows, and only a screenful built.
            let items: usize = groups
                .iter()
                .map(|group| 1 + group.shown + usize::from(group.pages))
                .sum();
            assert_eq!(page.items.len(), items);
            assert!(items < 10_000);
            let visible = app.dashboard(cx).read(cx).visible_rows();
            assert!(visible.len() <= 24, "built {} items", visible.len());
            eprintln!(
                "20 000 rows by host: {first:?} to build the page and draw (debug build), \
                 {items} items, {} built",
                visible.len()
            );

            // The end of the page: still a screenful, the last host's band
            // sticking to the top.
            app.keys(cx, "end");
            let visible = app.dashboard(cx).read(cx).visible_rows();
            assert!(visible.contains(&(page.items.len() - 1)), "{visible:?}");
            assert!(visible.len() <= 24);
            // ctrl-a marks every service, paged away or not.
            app.keys(cx, "ctrl-a");
            assert_eq!(app.marked(cx).len(), 20_000);
        },
    );
}
