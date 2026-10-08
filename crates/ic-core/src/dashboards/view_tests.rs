//! Multi-view dashboards (v1, topics 04 and 05): the handled switches per
//! kind, the dashboard's counts over its views, host-group grids, summary
//! tiles and event streams.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use ic_config::{
    Dashboard, DashboardGroup, Environment, GridCells, GridColour, GridOptions, GroupBy,
    GroupOrder, GroupSource, HandledMode, HandledSetting, HideHandled, ObjectKind, StreamOptions,
    View, ViewDisplay, ViewGroups,
};
use ic_model::{
    AckKind, CheckableState, HostState, ObjectKey, ServiceKey, ServiceState, StateType, Timestamp,
};
use ic_rules::DashboardRef;

use super::tests::{all, host, names, reference, s, sample, some, view};
use super::*;
use crate::command::{LogEntry, LogKind};
use crate::snapshot::{Grid, GridCell, Tile, ViewBody, ViewResult};
use crate::store::Changes;

/// An environment with one dashboard per entry, each with its views (ids
/// `v0`, `v1`, … unless set).
fn environment(dashboards: &[(&str, Vec<View>)]) -> Environment {
    Environment {
        groups: vec![DashboardGroup {
            id: "g".to_owned(),
            name: "group".to_owned(),
            dashboards: dashboards
                .iter()
                .map(|(id, views)| Dashboard {
                    id: (*id).to_owned(),
                    name: (*id).to_owned(),
                    views: views
                        .iter()
                        .enumerate()
                        .map(|(index, view)| View {
                            id: if view.id.is_empty() || view.id == "v" {
                                format!("v{index}")
                            } else {
                                view.id.clone()
                            },
                            ..view.clone()
                        })
                        .collect(),
                    ..Dashboard::default()
                })
                .collect(),
            ..DashboardGroup::default()
        }],
        ..Environment::default()
    }
}

fn evaluate_with(
    dashboards: &[(&str, Vec<View>)],
    data: &Data,
    defaults: HideHandled,
) -> Dashboards {
    let mut evaluated = Dashboards::default();
    evaluated.configure(&environment(dashboards), defaults);
    evaluated.update(data, &all(), false, &AtomicBool::new(false));
    evaluated
}

fn evaluate(dashboards: &[(&str, Vec<View>)], data: &Data) -> Dashboards {
    evaluate_with(dashboards, data, HideHandled::ALL)
}

fn dashboard<'a>(dashboards: &'a Dashboards, id: &str) -> &'a DashboardResult {
    &dashboards.results()[&reference(id)]
}

fn problems() -> View {
    View {
        problems_only: true,
        handled: HandledSetting::SETTINGS,
        ..view("")
    }
}

fn grid() -> View {
    View {
        display: ViewDisplay::HostGroupGrid,
        object_kind: ObjectKind::Hosts,
        ..view("")
    }
}

fn tiles() -> View {
    View {
        display: ViewDisplay::SummaryTiles,
        ..view("")
    }
}

fn grid_of(result: &ViewResult) -> &Grid {
    match &result.body {
        ViewBody::Grid(grid) => grid,
        other => panic!("not a grid: {other:?}"),
    }
}

fn tiles_of(result: &ViewResult) -> &[Tile] {
    match &result.body {
        ViewBody::Tiles(tiles) => tiles,
        other => panic!("not tiles: {other:?}"),
    }
}

fn events_of(result: &ViewResult) -> &Arc<Vec<LogEntry>> {
    match &result.body {
        ViewBody::Stream(events) => events,
        other => panic!("not a stream: {other:?}"),
    }
}

/// A grid's groups as `name: host state[hollow] …`.
fn grid_names(grid: &Grid) -> Vec<String> {
    grid.groups
        .iter()
        .map(|group| {
            let cells: Vec<String> = group.cells.iter().map(cell_name).collect();
            format!("{}: {}", group.name, cells.join(" "))
        })
        .collect()
}

fn cell_name(cell: &GridCell) -> String {
    let state = match cell.state {
        CheckableState::Host(state) => format!("{state:?}"),
        CheckableState::Service(state) => format!("{state:?}"),
    }
    .to_lowercase();
    let hollow = if cell.handled { "°" } else { "" };
    format!("{} {state}{hollow}", cell.host)
}

#[test]
fn handled_problems_hide_per_kind() {
    // The sample's problems: db-1!pg (acknowledged), db-1!disk, db-2!pg
    // (its host is down), web-1!http and lone!ssh.
    let data = sample();
    let hide = |hide: HideHandled| View {
        handled: HandledSetting {
            mode: HandledMode::Hide,
            hide,
        },
        ..problems()
    };
    let only_acknowledged = HideHandled {
        acknowledged: true,
        ..HideHandled::NONE
    };
    let only_host_down = HideHandled {
        host_down: true,
        ..HideHandled::NONE
    };
    let shown = View {
        handled: HandledSetting::SHOW,
        ..problems()
    };
    let dashboards = evaluate(
        &[
            ("settings", vec![problems()]),
            ("ack", vec![hide(only_acknowledged)]),
            ("down", vec![hide(only_host_down)]),
            ("show", vec![shown]),
        ],
        &data,
    );
    let first = |id| &dashboard(&dashboards, id).views[0];
    assert_eq!(
        names(first("settings")),
        ["web-1!http", "lone!ssh", "db-1!disk"]
    );
    assert_eq!(
        names(first("ack")),
        ["db-2!pg", "web-1!http", "lone!ssh", "db-1!disk"],
        "the service of a down host shows"
    );
    assert_eq!(
        names(first("down")),
        ["web-1!http", "lone!ssh", "db-1!disk", "db-1!pg"],
        "the acknowledged problem shows"
    );
    assert_eq!(
        names(first("show")),
        ["db-2!pg", "web-1!http", "lone!ssh", "db-1!disk", "db-1!pg"]
    );
    // `N hidden · show` and `N handled · hide`.
    let hidden: Vec<(u32, u32)> = ["settings", "ack", "down", "show"]
        .iter()
        .map(|id| (first(id).hidden, first(id).handled))
        .collect();
    assert_eq!(hidden, [(2, 2), (1, 2), (1, 2), (0, 2)]);
    // The header's counts are the unhandled ones: hiding or showing never
    // changes them; the rows' counts follow the rows.
    for id in ["settings", "ack", "down", "show"] {
        let counts = first(id).counts;
        assert_eq!(
            (
                counts.critical,
                counts.warning,
                counts.unknown,
                counts.unhandled
            ),
            (0, 2, 1, 3),
            "{id}"
        );
        assert_eq!(counts.handled, 0, "{id}");
    }
    assert_eq!(first("show").shown.critical, 2);
    assert_eq!(first("settings").shown.critical, 0);
    // The sidebar counts are the same whatever the view hides.
    for id in ["ack", "down", "show"] {
        assert_eq!(
            dashboard(&dashboards, id).summary,
            dashboard(&dashboards, "settings").summary
        );
    }
}

#[test]
fn a_downtime_hides_with_its_switch_whatever_the_state() {
    let mut data = sample();
    let services = Arc::make_mut(&mut data.services);
    for key in [
        ServiceKey::new("web-1", "http"),
        ServiceKey::new("web-1", "ssh"),
    ] {
        let mut service = (*services[&key]).clone();
        service.check.downtime_depth = 1;
        services.insert(key, Arc::new(service));
    }
    let every = View {
        problems_only: false,
        ..problems()
    };
    let downtimes_shown = HideHandled {
        in_downtime: false,
        ..HideHandled::ALL
    };
    let dashboards = evaluate_with(&[("d", vec![every])], &data, downtimes_shown);
    let result = &dashboard(&dashboards, "d").views[0];
    let rows = names(result);
    // Both in downtime show (the OK one too: a hollow green ring)...
    assert!(rows.contains(&"web-1!http".to_owned()), "{rows:?}");
    assert!(rows.contains(&"web-1!ssh".to_owned()), "{rows:?}");
    assert!(
        !rows.contains(&"db-1!pg".to_owned()),
        "acknowledged: hidden"
    );
    // ...and count as handled: not in the header's counts.
    assert_eq!(result.counts.unknown, 0);
    assert_eq!(result.hidden, 2, "db-1!pg and db-2!pg");
    assert_eq!(result.handled, 4);
    // The settings hiding downtimes again: only the switches change, the
    // views are restyled.
    let mut dashboards = dashboards;
    dashboards.configure(
        &environment(&[(
            "d",
            vec![View {
                problems_only: false,
                ..problems()
            }],
        )]),
        HideHandled::ALL,
    );
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    let rows = names(&dashboard(&dashboards, "d").views[0]);
    assert!(!rows.contains(&"web-1!ssh".to_owned()), "{rows:?}");
    assert_eq!(dashboard(&dashboards, "d").views[0].hidden, 4);
}

#[test]
fn a_dashboard_counts_each_object_of_its_views_once_but_not_its_streams() {
    let data = sample();
    let db = view("host.vars.role == \"db\"");
    let host_problems = View {
        object_kind: ObjectKind::Hosts,
        ..problems()
    };
    let stream = View {
        display: ViewDisplay::EventStream,
        ..view("")
    };
    let dashboards = evaluate(
        &[
            (
                "multi",
                vec![
                    db.clone(),
                    host_problems.clone(),
                    db.clone(),
                    stream.clone(),
                ],
            ),
            ("db", vec![db]),
            ("hosts", vec![host_problems]),
            ("stream", vec![stream]),
        ],
        &data,
    );
    let multi = dashboard(&dashboards, "multi");
    assert_eq!(multi.views.len(), 4);
    let ids: Vec<&str> = multi.views.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(ids, ["v0", "v1", "v2", "v3"]);
    assert_eq!(multi.view("v1").map(|view| view.id.as_str()), Some("v1"));
    let db = dashboard(&dashboards, "db").summary;
    let hosts = dashboard(&dashboards, "hosts").summary;
    // The db services twice and every host: each once.
    let summary = multi.summary;
    assert_eq!(summary.critical, db.critical);
    assert_eq!(summary.warning, db.warning);
    assert_eq!(summary.down, hosts.down);
    assert_eq!(summary.ok, db.ok + hosts.ok);
    assert_eq!(summary.unhandled, db.unhandled + hosts.unhandled);
    assert_eq!(summary.handled, db.handled + hosts.handled);
    // db-1!disk (warning) and db-2 (down): the redder wins.
    assert_eq!(
        summary.worst_unhandled,
        Some(CheckableState::Host(HostState::Down))
    );
    // Memberships: the views that count, not the stream.
    let streamed = reference("stream");
    assert!(
        !dashboards
            .memberships(&s("lone", "ssh"))
            .contains(&streamed)
    );
    assert_eq!(dashboard(&dashboards, "stream").summary, Summary::default());
    assert!(
        dashboards
            .memberships(&ObjectKey::host("db-2"))
            .contains(&reference("multi"))
    );
    assert!(
        dashboards
            .memberships(&s("db-1", "disk"))
            .contains(&reference("multi"))
    );
    assert!(
        !dashboards
            .memberships(&s("lone", "ssh"))
            .contains(&reference("multi"))
    );
}

#[test]
fn grids_colour_hosts_by_their_worst_problem() {
    let data = sample();
    let host_only = View {
        grid: GridOptions {
            colour: GridColour::HostOnly,
            ..GridOptions::default()
        },
        ..grid()
    };
    let dashboards = evaluate(&[("worst", vec![grid()]), ("host", vec![host_only])], &data);
    let worst = &dashboard(&dashboards, "worst").views[0];
    let cells = grid_of(worst);
    // Worst first: both have one down host (red) as their worst problem
    // (the reddest state decides, not Icinga's severity, which weighs web's
    // unknown above a down host), so they go by name.
    assert_eq!(
        grid_names(cells),
        ["db: db-1 warning db-2 down", "web: db-2 down web-1 unknown"]
    );
    assert_eq!(cells.hosts, 3, "lone is in no host group");
    assert_eq!(cells.groups[1].label, "Web servers");
    assert_eq!(
        cells.groups[1].counts.worst_unhandled,
        Some(CheckableState::Host(HostState::Down)),
        "the down host, not the unknown service"
    );
    let db_1 = &cells.groups[0].cells[0];
    assert_eq!(db_1.worst_service.as_deref(), Some("disk"));
    assert_eq!(db_1.problems, 1, "the acknowledged pg doesn't count");
    // A down host's square is the host's: its services are handled by it.
    let db_2 = &cells.groups[0].cells[1];
    assert_eq!(db_2.worst_service, None);
    // The group headers count the squares that aren't handled.
    let db = cells.groups[0].counts;
    assert_eq!((db.warning, db.down, db.unhandled), (1, 1, 2));
    assert_eq!(
        db.worst_unhandled,
        Some(CheckableState::Host(HostState::Down))
    );
    // The header: each host once; the summary: the hosts and their
    // services.
    assert_eq!(
        (
            worst.counts.warning,
            worst.counts.down,
            worst.counts.unknown
        ),
        (1, 1, 1)
    );
    assert_eq!(worst.summary.critical, 2);
    assert_eq!(worst.summary.unhandled, 3);
    // The services of a host on the grid belong to the dashboard.
    assert!(
        dashboards
            .memberships(&s("db-1", "disk"))
            .contains(&reference("worst"))
    );
    assert!(
        !dashboards
            .memberships(&s("lone", "ssh"))
            .contains(&reference("worst"))
    );
    assert!(
        !dashboards
            .memberships(&s("db-1", "disk"))
            .contains(&reference("host"))
    );
    assert_eq!(dashboard(&dashboards, "worst").summary, worst.summary);

    let host = grid_of(&dashboard(&dashboards, "host").views[0]);
    assert_eq!(
        grid_names(host),
        ["db: db-1 up db-2 down", "web: db-2 down web-1 up"],
        "equal groups by name"
    );
}

#[test]
fn handled_hosts_are_hollow_even_when_ok() {
    let mut data = sample();
    let hosts = Arc::make_mut(&mut data.hosts);
    let mut web = host("web-1", HostState::Up, &["web"], "web");
    web.check.downtime_depth = 1;
    hosts.insert("web-1".into(), Arc::new(web));
    let mut db = host("db-2", HostState::Down, &["db", "web"], "db");
    db.check.acknowledgement = AckKind::Normal;
    hosts.insert("db-2".into(), Arc::new(db));
    let services = Arc::make_mut(&mut data.services);
    let key = ServiceKey::new("web-1", "http");
    let mut http = (*services[&key]).clone();
    http.state = ServiceState::Ok;
    services.insert(key, Arc::new(http));
    let dashboards = evaluate(&[("g", vec![grid()])], &data);
    let result = &dashboard(&dashboards, "g").views[0];
    assert_eq!(
        grid_names(grid_of(result)),
        ["db: db-1 warning db-2 down°", "web: db-2 down° web-1 up°"]
    );
    assert_eq!(result.handled, 2);
    assert_eq!(result.counts.unhandled, 1, "only db-1's warning");
}

#[test]
fn grids_pick_their_groups() {
    let data = sample();
    let picked = View {
        groups: ViewGroups {
            host_groups: vec!["w*".to_owned()],
            ..ViewGroups::default()
        },
        ..grid()
    };
    let by_role = View {
        groups: ViewGroups {
            by: GroupSource::CustomVar,
            custom_var: "host.vars.role".to_owned(),
            order: GroupOrder::Name,
            ..ViewGroups::default()
        },
        grid: GridOptions {
            cells: GridCells::LabelledCells,
            ..GridOptions::default()
        },
        ..grid()
    };
    let first_group_only = View {
        grid: GridOptions {
            colour: GridColour::HostOnly,
            hide_healthy_groups: true,
            host_in_each_group: false,
            ..GridOptions::default()
        },
        ..grid()
    };
    let filtered = View {
        filter: "host.state == 0".to_owned(),
        ..grid()
    };
    let dashboards = evaluate(
        &[
            ("picked", vec![picked]),
            ("role", vec![by_role]),
            ("first", vec![first_group_only]),
            ("filtered", vec![filtered]),
        ],
        &data,
    );
    let names_of = |id| grid_names(grid_of(&dashboard(&dashboards, id).views[0]));
    assert_eq!(names_of("picked"), ["web: db-2 down web-1 unknown"]);
    assert!(
        !dashboards
            .memberships(&ObjectKey::host("db-1"))
            .contains(&reference("picked"))
    );
    assert_eq!(
        names_of("role"),
        [
            "db: db-1 warning db-2 down",
            "misc: lone warning",
            "web: web-1 unknown"
        ]
    );
    // db-2 only in its first group; web then has nothing but OK hosts.
    assert_eq!(names_of("first"), ["db: db-1 up db-2 down"]);
    assert_eq!(
        names_of("filtered"),
        ["web: web-1 unknown", "db: db-1 warning"]
    );
}

#[test]
fn tiles_count_their_groups() {
    let data = sample();
    let by_name = View {
        groups: ViewGroups {
            order: GroupOrder::Name,
            ..ViewGroups::default()
        },
        ..tiles()
    };
    let host_tiles = View {
        object_kind: ObjectKind::Hosts,
        ..tiles()
    };
    let dashboards = evaluate(
        &[
            ("worst", vec![tiles()]),
            ("name", vec![by_name]),
            ("hosts", vec![host_tiles]),
        ],
        &data,
    );
    let result = &dashboard(&dashboards, "worst").views[0];
    let tiles = tiles_of(result);
    let labels: Vec<&str> = tiles.iter().map(|tile| tile.label.as_str()).collect();
    assert_eq!(
        labels,
        ["Web servers", "Databases"],
        "unknown before warning"
    );
    let databases = &tiles[1];
    assert_eq!((databases.name.as_str(), databases.hosts), ("db", 2));
    let summary = databases.summary;
    assert_eq!(
        (
            summary.critical,
            summary.warning,
            summary.ok,
            summary.handled
        ),
        (2, 1, 2, 2)
    );
    assert_eq!(
        summary.worst_unhandled,
        Some(CheckableState::Service(ServiceState::Warning))
    );
    // The numbers leave the handled problems out (the acknowledged pg, and
    // db-2's pg, handled by its down host), as the view header does.
    let counts = databases.counts;
    assert_eq!(
        (counts.critical, counts.warning, counts.ok, counts.handled),
        (0, 1, 2, 0)
    );
    assert_eq!(
        counts.worst_unhandled,
        Some(CheckableState::Service(ServiceState::Warning))
    );
    // The view counts each service once (db-2's are in both tiles).
    assert_eq!(result.summary.critical, 2);
    assert_eq!(result.summary.unhandled + result.summary.handled, 4);
    assert_eq!(result.handled, 2);
    assert_eq!((result.counts.warning, result.counts.unknown), (1, 1));
    let by_name = tiles_of(&dashboard(&dashboards, "name").views[0]);
    assert_eq!(by_name[0].label, "Databases");
    let hosts = tiles_of(&dashboard(&dashboards, "hosts").views[0]);
    assert_eq!(hosts[1].summary.down, 1);
    assert_eq!(hosts[1].hosts, 2);
}

#[test]
fn groups_follow_their_reddest_count() {
    // x: a host with a critical service that is unreachable (Icinga's
    // severity puts it below any unhandled warning) and a host with an
    // unknown; y: one warning; z: two hosts with a warning each.
    let mut unreachable = tests::service("a", "crit", ServiceState::Critical, 1.0);
    unreachable.check.reachable = false;
    let data = tests::data(
        vec![
            host("a", HostState::Up, &["x"], "r"),
            host("e", HostState::Up, &["x"], "r"),
            host("b", HostState::Up, &["y"], "r"),
            host("c", HostState::Up, &["z"], "r"),
            host("d", HostState::Up, &["z"], "r"),
        ],
        vec![
            unreachable,
            tests::service("e", "unknown", ServiceState::Unknown, 1.0),
            tests::service("b", "warning", ServiceState::Warning, 1.0),
            tests::service("c", "warning", ServiceState::Warning, 1.0),
            tests::service("d", "warning", ServiceState::Warning, 1.0),
        ],
    );
    let dashboards = evaluate(&[("grid", vec![grid()]), ("tiles", vec![tiles()])], &data);
    let critical = Some(CheckableState::Service(ServiceState::Critical));
    // Red before yellow, then more of the same colour first.
    let grid = grid_of(&dashboard(&dashboards, "grid").views[0]);
    let names: Vec<&str> = grid
        .groups
        .iter()
        .map(|group| group.name.as_str())
        .collect();
    assert_eq!(names, ["x", "z", "y"]);
    assert_eq!(grid.groups[0].counts.worst_unhandled, critical);
    let tiles = tiles_of(&dashboard(&dashboards, "tiles").views[0]);
    let names: Vec<&str> = tiles.iter().map(|tile| tile.name.as_str()).collect();
    assert_eq!(names, ["x", "z", "y"]);
    assert_eq!(tiles[0].counts.worst_unhandled, critical);
    assert_eq!((tiles[0].counts.critical, tiles[0].counts.unknown), (1, 1));
}

fn entry(seconds: f64, object: ObjectKey, kind: LogKind) -> LogEntry {
    LogEntry {
        at: Timestamp::from_unix_seconds(seconds),
        object,
        kind,
        text: String::new(),
        author: None,
    }
}

fn state(state: ServiceState, state_type: StateType) -> LogKind {
    LogKind::State {
        state: CheckableState::Service(state),
        state_type,
    }
}

#[test]
fn event_streams_show_the_events_of_their_objects() {
    let mut data = sample();
    // Newest first, as the engine keeps them.
    data.events = Arc::new(vec![
        entry(
            900.0,
            s("db-1", "disk"),
            state(ServiceState::Warning, StateType::Hard),
        ),
        entry(800.0, ObjectKey::host("db-2"), LogKind::FlappingStarted),
        entry(
            700.0,
            s("db-1", "ssh"),
            state(ServiceState::Ok, StateType::Hard),
        ),
        entry(600.0, s("db-1", "pg"), LogKind::AcknowledgementSet),
        entry(
            500.0,
            s("lone", "ssh"),
            state(ServiceState::Warning, StateType::Hard),
        ),
        entry(
            400.0,
            s("db-1", "disk"),
            state(ServiceState::Warning, StateType::Soft),
        ),
    ]);
    let stream = View {
        display: ViewDisplay::EventStream,
        ..view("host.vars.role == \"db\"")
    };
    let everything = View {
        stream: StreamOptions {
            hard_states_only: false,
            recoveries: true,
            events: ic_config::StreamEvents {
                flapping: true,
                ..ic_config::StreamEvents::default()
            },
            ..StreamOptions::default()
        },
        ..stream.clone()
    };
    let mut dashboards = evaluate(&[("s", vec![stream]), ("all", vec![everything])], &data);
    let times = |dashboards: &Dashboards, id| -> Vec<f64> {
        events_of(&dashboard(dashboards, id).views[0])
            .iter()
            .map(|entry| entry.at.as_unix_seconds())
            .collect()
    };
    // Hard problems and acknowledgements of the db hosts and services.
    assert_eq!(times(&dashboards, "s"), [900.0, 600.0]);
    assert_eq!(
        times(&dashboards, "all"),
        [900.0, 800.0, 700.0, 600.0, 400.0]
    );
    let before = Arc::clone(events_of(&dashboard(&dashboards, "s").views[0]));
    // Nothing new: the same events.
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert!(Arc::ptr_eq(
        &before,
        events_of(&dashboard(&dashboards, "s").views[0])
    ));
    // A new event comes first.
    let mut events = (*data.events).clone();
    events.insert(
        0,
        entry(1_000.0, ObjectKey::host("db-1"), LogKind::DowntimeStarted),
    );
    data.events = Arc::new(events);
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert_eq!(times(&dashboards, "s"), [1_000.0, 900.0, 600.0]);
    // An object that joins the filter brings its events.
    let hosts = Arc::make_mut(&mut data.hosts);
    hosts.insert(
        "lone".into(),
        Arc::new(host("lone", HostState::Up, &[], "db")),
    );
    dashboards.update(
        &data,
        &some(&[ObjectKey::host("lone")]),
        false,
        &AtomicBool::new(false),
    );
    assert_eq!(times(&dashboards, "s"), [1_000.0, 900.0, 600.0, 500.0]);
    // A stream has no counts.
    assert_eq!(
        dashboard(&dashboards, "s").views[0].counts,
        Summary::default()
    );
}

#[test]
fn views_are_matched_by_id() {
    let data = sample();
    let a = View {
        id: "a".to_owned(),
        ..problems()
    };
    let b = View {
        id: "b".to_owned(),
        ..grid()
    };
    let mut dashboards = evaluate(&[("d", vec![a.clone(), b.clone()])], &data);
    let rows = match &dashboard(&dashboards, "d").views[0].body {
        ViewBody::List(rows) => Arc::clone(rows),
        other => panic!("{other:?}"),
    };
    // Reordered: the results follow, and the list keeps its rows.
    dashboards.configure(
        &environment(&[("d", vec![b.clone(), a.clone()])]),
        HideHandled::ALL,
    );
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    let result = dashboard(&dashboards, "d");
    let ids: Vec<&str> = result.views.iter().map(|view| view.id.as_str()).collect();
    assert_eq!(ids, ["b", "a"]);
    match &result.view("a").unwrap().body {
        ViewBody::List(after) => assert!(Arc::ptr_eq(&rows, after)),
        other => panic!("{other:?}"),
    }
    // Removed.
    dashboards.configure(&environment(&[("d", vec![a])]), HideHandled::ALL);
    dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert_eq!(dashboard(&dashboards, "d").views.len(), 1);
    assert!(dashboard(&dashboards, "d").view("b").is_none());
}

#[test]
fn multi_view_updates_match_full_evaluations() {
    let dashboards_of = || {
        vec![
            (
                "m",
                vec![
                    grid(),
                    View {
                        groups: ViewGroups {
                            by: GroupSource::CustomVar,
                            custom_var: "role".to_owned(),
                            ..ViewGroups::default()
                        },
                        ..tiles()
                    },
                    problems(),
                    {
                        let mut grouped = view("");
                        grouped.set_grouping(GroupBy::HostGroup);
                        grouped
                    },
                    View {
                        display: ViewDisplay::EventStream,
                        ..view("host.vars.role == \"web\"")
                    },
                ],
            ),
            (
                "g",
                vec![View {
                    grid: GridOptions {
                        hide_healthy_groups: true,
                        ..GridOptions::default()
                    },
                    ..grid()
                }],
            ),
        ]
    };
    let mut data = sample();
    let mut dashboards = evaluate(&dashboards_of(), &data);
    let check = |dashboards: &mut Dashboards, data: &mut Data, dirty: Vec<ObjectKey>| {
        dashboards.update(data, &some(&dirty), false, &AtomicBool::new(false));
        let full = evaluate(&dashboards_of(), data);
        assert_eq!(**dashboards.results(), **full.results());
        for object in data
            .hosts
            .keys()
            .map(|name| ObjectKey::host(name.as_str()))
            .chain(data.services.keys().cloned().map(ObjectKey::from))
        {
            assert_eq!(
                dashboards.memberships(&object),
                full.memberships(&object),
                "{object}"
            );
        }
    };
    // A service of a grid host turns critical: its square turns red.
    let key = ServiceKey::new("web-1", "ssh");
    let services = Arc::make_mut(&mut data.services);
    let mut ssh = (*services[&key]).clone();
    ssh.state = ServiceState::Critical;
    services.insert(key.clone(), Arc::new(ssh));
    check(&mut dashboards, &mut data, vec![key.into()]);
    let cell = &grid_of(&dashboard(&dashboards, "m").views[0]).groups[0].cells;
    assert!(
        cell.iter()
            .any(|cell| cell.worst_service.as_deref() == Some("ssh"))
    );
    // A host changes groups and recovers.
    Arc::make_mut(&mut data.hosts).insert(
        "db-2".into(),
        Arc::new(host("db-2", HostState::Up, &["web"], "web")),
    );
    check(&mut dashboards, &mut data, vec![ObjectKey::host("db-2")]);
    // A host joins a group; a service is gone.
    Arc::make_mut(&mut data.hosts).insert(
        "lone".into(),
        Arc::new(host("lone", HostState::Up, &["db"], "misc")),
    );
    Arc::make_mut(&mut data.services).remove(&ServiceKey::new("db-1", "disk"));
    check(
        &mut dashboards,
        &mut data,
        vec![ObjectKey::host("lone"), s("db-1", "disk")],
    );
}

#[test]
fn quiet_mode_leaves_streams_for_later() {
    let data = sample();
    let stream = View {
        display: ViewDisplay::EventStream,
        ..view("")
    };
    let mut dashboards = Dashboards::default();
    dashboards.configure(
        &environment(&[("d", vec![problems(), stream])]),
        HideHandled::ALL,
    );
    dashboards.set_scope(Scope::Quiet(None));
    let results = dashboards.update(&data, &all(), false, &AtomicBool::new(false));
    assert!(results.is_empty(), "no result while quiet");
    assert!(
        dashboards
            .memberships(&s("db-1", "disk"))
            .contains(&reference("d"))
    );
    dashboards.set_scope(Scope::All);
    let results = dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert_eq!(results[&reference("d")].views.len(), 2);
}

#[test]
fn quiet_mode_leaves_the_sidebar_union_to_be_rebuilt() {
    // Two counting views and no stream: nothing else forces a full
    // evaluation when quiet mode ends.
    let views = vec![problems(), view("host.name == \"db-1\"")];
    for only in [
        None,
        Some([reference("d")].into_iter().collect::<BTreeSet<_>>()),
    ] {
        let mut data = sample();
        let mut dashboards = evaluate(&[("d", views.clone())], &data);
        dashboards.set_scope(Scope::Quiet(only));

        // ssh on db-1 goes critical while quiet.
        let key = ServiceKey::new("db-1", "ssh");
        let services = Arc::make_mut(&mut data.services);
        let mut ssh = (*services[&key]).clone();
        ssh.state = ServiceState::Critical;
        services.insert(key.clone(), Arc::new(ssh));
        dashboards.update(
            &data,
            &some(&[ObjectKey::from(key)]),
            false,
            &AtomicBool::new(false),
        );

        // Awake, with nothing new: the sidebar's counts take the change.
        dashboards.set_scope(Scope::All);
        dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
        let fresh = evaluate(&[("d", views.clone())], &data);
        assert_eq!(
            dashboard(&dashboards, "d").summary,
            dashboard(&fresh, "d").summary
        );
        assert_eq!(dashboard(&dashboards, "d"), dashboard(&fresh, "d"));
    }
}

#[test]
fn previews_and_fixtures_evaluate_like_the_engine() {
    let data = sample();
    let views = vec![problems(), grid(), tiles()];
    let engine = evaluate(&[("d", views.clone())], &data);
    let ids: Vec<View> = views
        .into_iter()
        .enumerate()
        .map(|(index, view)| View {
            id: format!("v{index}"),
            ..view
        })
        .collect();
    let preview = preview(&ids, &data, HideHandled::ALL);
    assert_eq!(&preview, dashboard(&engine, "d"));
    let snapshot = Snapshot {
        hosts: Arc::clone(&data.hosts),
        services: Arc::clone(&data.services),
        host_groups: Arc::clone(&data.host_groups),
        service_groups: Arc::clone(&data.service_groups),
        taken_at: data.now,
        ..Snapshot::default()
    };
    assert_eq!(
        evaluate_dashboard(&ids, &snapshot, &[], HideHandled::ALL),
        preview
    );
}

#[test]
fn unknown_view_references_are_fine() {
    let reference = DashboardRef {
        group_id: "g".to_owned(),
        dashboard_id: "missing".to_owned(),
    };
    let dashboards = evaluate(&[("d", vec![problems()])], &sample());
    assert!(!dashboards.results().contains_key(&reference));
}

fn members_of(result: &ViewResult) -> &Arc<BTreeSet<ObjectKey>> {
    result.members().expect("a handling or downtimes view")
}

#[test]
fn handling_and_downtimes_views_hold_their_filters_hosts_and_services() {
    let data = sample();
    let handling = View {
        display: ViewDisplay::Handling,
        ..view("host.vars.role == \"db\"")
    };
    let downtimes = View {
        display: ViewDisplay::Downtimes,
        ..view("service.name == \"ssh\"")
    };
    let dashboards = evaluate(
        &[
            ("handling", vec![handling.clone()]),
            ("downtimes", vec![downtimes]),
            ("mixed", vec![view("host.vars.role == \"web\""), handling]),
        ],
        &data,
    );
    let handling = &dashboard(&dashboards, "handling").views[0];
    // The db hosts and every one of their services.
    let names: Vec<String> = members_of(handling)
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        names,
        [
            "db-1",
            "db-2",
            "db-1!disk",
            "db-1!pg",
            "db-1!ssh",
            "db-2!pg",
            "db-2!ssh"
        ],
        "{names:?}"
    );
    assert_eq!((handling.hosts, handling.services), (2, 5));
    // A host has no `service.name`: the filter picks services only.
    let downtimes = &dashboard(&dashboards, "downtimes").views[0];
    assert_eq!(members_of(downtimes).len(), 4);
    assert_eq!((downtimes.hosts, downtimes.services), (0, 4));
    // Neither counts: no sidebar number, no notifications.
    assert_eq!(
        dashboard(&dashboards, "handling").summary,
        Summary::default()
    );
    assert!(
        !dashboards
            .memberships(&ObjectKey::host("db-2"))
            .contains(&reference("handling"))
    );
    // Beside a list, the dashboard counts the list's objects only.
    let mixed = dashboard(&dashboards, "mixed");
    assert_eq!(mixed.summary.unknown, 1, "web-1!http");
    assert_eq!(mixed.summary.critical, 0, "db-1!pg is the handling view's");
    assert!(
        !dashboards
            .memberships(&s("db-1", "pg"))
            .contains(&reference("mixed"))
    );
}

#[test]
fn members_keep_their_arc_until_someone_joins_or_leaves() {
    let data = sample();
    let handling = View {
        display: ViewDisplay::Handling,
        ..view("service.state != 0")
    };
    let mut dashboards = evaluate(&[("h", vec![handling])], &data);
    let before = Arc::clone(members_of(&dashboard(&dashboards, "h").views[0]));
    // pg on db-1 goes from critical to warning: still a member.
    let mut services = (*data.services).clone();
    let key = ServiceKey::new("db-1", "pg");
    let mut pg = (*services[&key]).clone();
    pg.state = ServiceState::Warning;
    services.insert(key.clone(), Arc::new(pg));
    let changed = Data {
        services: Arc::new(services),
        ..data.clone()
    };
    dashboards.update(
        &changed,
        &some(&[ObjectKey::from(key.clone())]),
        false,
        &AtomicBool::new(false),
    );
    let after = members_of(&dashboard(&dashboards, "h").views[0]);
    assert!(Arc::ptr_eq(&before, after));
    // It recovers: it leaves.
    let mut services = (*changed.services).clone();
    let mut pg = (*services[&key]).clone();
    pg.state = ServiceState::Ok;
    services.insert(key.clone(), Arc::new(pg));
    let recovered = Data {
        services: Arc::new(services),
        ..changed
    };
    dashboards.update(
        &recovered,
        &some(&[ObjectKey::from(key.clone())]),
        false,
        &AtomicBool::new(false),
    );
    let after = members_of(&dashboard(&dashboards, "h").views[0]);
    assert!(!after.contains(&ObjectKey::from(key)));
    assert_eq!(after.len(), before.len() - 1);
}

#[test]
fn drawing_choices_keep_the_result() {
    let data = sample();
    let list = View {
        id: "l".to_owned(),
        ..view("")
    };
    let mut environment = environment(&[("d", vec![list.clone()])]);
    let mut dashboards = Dashboards::default();
    dashboards.configure(&environment, HideHandled::ALL);
    dashboards.update(&data, &all(), false, &AtomicBool::new(false));
    let before = dashboards.results().clone();
    // The row density and the threads' options are the app's to draw.
    let view = &mut environment.groups[0].dashboards[0].views[0];
    view.density = Some(ic_config::RowDensity::Compact);
    view.threads.only_mine = true;
    dashboards.configure(&environment, HideHandled::ALL);
    let after = dashboards.update(&data, &Changes::default(), false, &AtomicBool::new(false));
    assert!(Arc::ptr_eq(&before, &after), "nothing evaluated again");
}

#[test]
fn a_state_chip_shows_one_state_and_keeps_the_counts() {
    let data = sample();
    let all_states = View {
        problems_only: true,
        ..view("")
    };
    let warnings = View {
        state: Some(ic_config::StateChip::Warning),
        ..all_states.clone()
    };
    let dashboards = evaluate(
        &[("all", vec![all_states]), ("warnings", vec![warnings])],
        &data,
    );
    let all_states = &dashboard(&dashboards, "all").views[0];
    let warnings = &dashboard(&dashboards, "warnings").views[0];
    assert_eq!(
        names(warnings),
        ["lone!ssh", "db-1!disk"],
        "only the warnings show"
    );
    // The header's numbers (and the sidebar's) don't change with the chip.
    assert_eq!(warnings.counts, all_states.counts);
    assert_eq!(
        dashboard(&dashboards, "warnings").summary,
        dashboard(&dashboards, "all").summary
    );
    assert_eq!(warnings.shown.warning, 2);
    assert_eq!(warnings.shown.critical, 0);
}

#[test]
fn the_cluster_events_are_the_streams_without_a_filter() {
    let critical = entry(
        30.,
        s("db-1", "pg"),
        state(ServiceState::Critical, StateType::Hard),
    );
    let soft = entry(
        20.,
        s("db-1", "disk"),
        state(ServiceState::Warning, StateType::Soft),
    );
    let ack = entry(10., s("db-1", "pg"), LogKind::AcknowledgementSet);
    let events = vec![critical.clone(), soft, ack.clone()];
    assert_eq!(
        stream_events(StreamOptions::default(), &events),
        [critical, ack]
    );
}
