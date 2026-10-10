//! Knowing when icygui is blind (PLAN.md §4.2 A, B, B3, H, no false
//! green) through the app: the demo's heartbeats are found, watched on the
//! stream and shown on the health page, and stay out of every search; no
//! live data shows on every page, in the footer and the tray; the health
//! page is edited in the dashboard editor as a built-in dashboard of its
//! own kinds, and *reset to default* brings 06's layout back.

use std::sync::Arc;
use std::time::Duration;

use futures::FutureExt as _;
use gpui::{App, Entity, Modifiers, point, px};
use ic_config::{HealthPage, ViewDisplay};
use ic_core::ConnectionState;
use ic_core::heartbeat::{BeatState, HeartbeatSetup};
use ic_core::snapshot::Snapshot;
use ic_core::trouble::{Alert, AlertAction, AlertTone, Blind, Trouble};
use ic_model::Timestamp;

use super::environments::{CONNECT, connected_to};
use super::{Body, Harness, run, run_app, wait_for};
use crate::app_state::{ConnectionStatus, Health, NoticeKind};
use crate::cluster::beats::BeatTone;
use crate::cluster::health::{Dot, EndpointRow, Tone};
use crate::cluster::{ClusterEntry, HealthPageEvent, HealthStop};
use crate::editor::DashboardEditor;
use crate::fixture::FixtureOptions;
use crate::live::demo;
use crate::palette::PaletteCommand;

/// The editor, open.
fn editor(app: &Harness, cx: &App) -> Entity<DashboardEditor> {
    app.workspace.read(cx).editor().expect("the editor").clone()
}

/// Shows the cluster health page from its entry in the sidebar's
/// cluster section (the fourth row under the 41px header and the
/// section's 36px heading).
fn show_health(app: &Harness, cx: &mut App) {
    let index = ClusterEntry::ALL
        .iter()
        .position(|entry| *entry == ClusterEntry::Health)
        .unwrap();
    #[expect(clippy::cast_precision_loss, reason = "four rows")]
    let row = 30. * index as f32;
    app.click(
        cx,
        point(px(150.), px(41. + 36. + 15. + row)),
        Modifiers::default(),
    );
    assert_eq!(app.workspace.read(cx).shown_page(), "health");
}

/// B, B3: the demo's six heartbeats (16a: one pinned per master, one per
/// satellite zone, one pinned per satellite of the HA zone fra) are found
/// by their custom variable, beat on the live stream, and show on the
/// health page's heartbeat row and in its table; their objects stay out
/// of the palette's search.
#[test]
#[expect(clippy::too_many_lines, reason = "one story, step by step")]
fn heartbeats_are_found_shown_and_kept_out_of_searches() {
    run_app(
        crate::WINDOW_SIZE,
        super::environments::demo_app(None),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "prod-cluster", CONNECT, |app, cx| {
                    connected_to(app.state.read(cx), demo::ENDPOINT)
                })
                .await;
                // The demo's beats run every 30 s: each arrives on the
                // stream within that.
                wait_for(
                    &app,
                    &cx,
                    "every beat on time",
                    Duration::from_secs(75),
                    |app, cx| {
                        let beats = &app.state.read(cx).snapshot().heartbeats;
                        beats.beats.len() == 6
                            && beats
                                .beats
                                .iter()
                                .all(|beat| beat.state == BeatState::OnTime)
                    },
                )
                .await;
                cx.update(|cx| {
                    let state = app.state.read(cx);
                    let snapshot = state.snapshot();
                    assert_eq!(
                        snapshot.heartbeats.setup,
                        HeartbeatSetup::Find {
                            variable: "icygui_heartbeat".to_owned()
                        }
                    );
                    assert!(!snapshot.heartbeats.polled, "a live stream carries them");
                    let mut proves: Vec<String> = snapshot
                        .heartbeats
                        .beats
                        .iter()
                        .map(|beat| beat.proves.label())
                        .collect();
                    proves.sort();
                    assert_eq!(
                        proves,
                        [
                            "master-01",
                            "master-02",
                            "sat-fra-01",
                            "sat-fra-02",
                            "zone ams",
                            "zone fra"
                        ]
                    );
                    assert_eq!(snapshot.excluded.len(), 6, "left out of everything");
                });
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        state.show_cluster(ClusterEntry::Health);
                        cx.notify();
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(app.workspace.read(cx).shown_page(), "health");
                    let page = app.workspace.read(cx).health_page().clone();
                    let report = page.read(cx).report(cx);
                    assert_eq!(report.beats.label, "heartbeats");
                    assert_eq!(report.beats.subject, "6 of 6");
                    assert_eq!(report.beats.status, "on time");
                    assert!(report.alerts.is_empty(), "{:?}", report.alerts);
                    // A zone's beat on its zone's line, a pinned beat on
                    // its endpoint's (16a): the masters pinned only, the
                    // HA satellite zone fra both.
                    let master = &report.zones[0];
                    assert_eq!(master.name, "master");
                    assert!(master.beat.is_none());
                    assert!(master.endpoints.iter().all(|row| row.beat.is_some()));
                    let ams = &report.zones[1];
                    assert!(ams.beat.is_some());
                    assert!(ams.endpoints.iter().all(|row| row.beat.is_none()));
                    let fra = &report.zones[2];
                    assert!(fra.beat.is_some());
                    let pinned: Vec<&str> = fra
                        .endpoints
                        .iter()
                        .filter(|row| row.beat.is_some())
                        .map(|row| row.name.as_str())
                        .collect();
                    assert_eq!(pinned, ["sat-fra-01", "sat-fra-02"]);
                });
                // The palette doesn't find them.
                cx.update(|cx| app.keys(cx, "ctrl-k"));
                cx.update(|cx| {
                    let palette = app.workspace.read(cx).palette().unwrap().clone();
                    let input = palette.read(cx).input().clone();
                    app.in_window(cx, |window, cx| {
                        input.update(cx, |input, cx| input.replace_all("beat", window, cx));
                    });
                    app.draw(cx);
                    let items = palette.read(cx).items().to_vec();
                    assert!(
                        !items.iter().any(|item| matches!(
                            &item.command,
                            PaletteCommand::OpenObject(key)
                                if key.as_service().is_some_and(|key| key.host.to_string().starts_with("icygui-hb-"))
                        )),
                        "{:?}",
                        items.iter().map(|item| &item.label).collect::<Vec<_>>()
                    );
                    app.keys(cx, "escape");
                });
            }
            .boxed_local()
        })),
    );
}

/// `snapshot` with the fixture's endpoints in their zones (master, ams
/// and fra under it), so the health page has its table.
fn with_zones(snapshot: &Snapshot) -> Snapshot {
    let zone = |name: &str, parent: Option<&str>, endpoint: &str| ic_model::Zone {
        name: name.to_owned(),
        parent: parent.map(str::to_owned),
        endpoints: vec![endpoint.to_owned()],
        global: false,
    };
    Snapshot {
        zones: Arc::new(vec![
            zone("master", None, "master-01"),
            zone("ams", Some("master"), "sat-ams-01"),
            zone("fra", Some("master"), "sat-fra-01"),
        ]),
        ..snapshot.clone()
    }
}

/// A snapshot whose engine says the environment has had no live data
/// since `since`, because the stream stalled.
fn blind_since(snapshot: &Snapshot, since: Timestamp) -> Snapshot {
    let snapshot = &with_zones(snapshot);
    Snapshot {
        trouble: Arc::new(Trouble {
            alerts: Vec::new(),
            blind: Some(Blind {
                since,
                reason: "event stream stalled".to_owned(),
            }),
        }),
        ..snapshot.clone()
    }
}

/// No false green in the health page's endpoint rows without live data:
/// the statuses are the last known ones, marked as of when they came; the
/// node icygui talks to isn't, its row says what icygui knows of its own
/// connection (silent, then lost) in the banner's tone.
fn endpoint_statuses_are_last_known(app: &Harness, cx: &mut App, now: Timestamp, since: Timestamp) {
    let page = app.workspace.read(cx).health_page().clone();
    let report = page.read(cx).report(cx);
    let rows: Vec<&EndpointRow> = report
        .zones
        .iter()
        .flat_map(|zone| &zone.endpoints)
        .collect();
    let (this_node, others): (Vec<&EndpointRow>, Vec<&EndpointRow>) =
        rows.into_iter().partition(|row| row.this_node);
    assert_eq!(this_node.len(), 1, "{this_node:?}");
    assert_eq!(
        (
            this_node[0].status.as_str(),
            this_node[0].tone,
            this_node[0].as_of.as_deref()
        ),
        ("no live data · this node", Tone::Warning, None)
    );
    assert!(report.as_of.is_some());
    assert!(!others.is_empty());
    for row in others {
        assert_eq!(row.as_of, report.as_of, "{}", row.name);
    }
    // The connection lost (reconnecting): the row says that.
    app.state.update(cx, |state, cx| {
        let mut status = state.connection().clone();
        status.on_state(ConnectionState::Reconnecting {
            error: "connection reset".to_owned(),
            attempt: 2,
            retry_at: Timestamp::from_unix_seconds(now.as_unix_seconds() + 30.),
            untrusted: None,
        });
        state.set_connection(status);
        cx.notify();
    });
    app.draw(cx);
    let report = page.read(cx).report(cx);
    let master = report
        .zones
        .iter()
        .flat_map(|zone| &zone.endpoints)
        .find(|row| row.this_node)
        .expect("this node");
    assert_eq!(
        (master.status.as_str(), master.tone, master.as_of.as_deref()),
        ("connection lost · this node", Tone::Warning, None)
    );
    app.state.update(cx, |state, cx| {
        let mut status = state.connection().clone();
        status.on_state(ConnectionState::Connected {
            node: crate::app_state::connection::full_node("master-01"),
            version: "r2.15.6-1".to_owned(),
            since,
        });
        state.set_connection(status);
        cx.notify();
    });
}

/// A: no live data for three minutes shows on every page (the banner),
/// in the footer (`no data 3m`, the warning colour, never green) and in
/// the tray (its blind look, the environment's line), and goes once the
/// engine says it is live again.
#[test]
fn no_live_data_shows_on_every_page_the_footer_and_the_tray() {
    run(FixtureOptions::default(), |app, cx| {
        let now = Timestamp::now();
        let since = Timestamp::from_unix_seconds(now.as_unix_seconds() - 180.);
        let live = app.state.read(cx).snapshot().clone();
        let blind = Snapshot {
            node: Some(Arc::new(crate::app_state::connection::full_node(
                "master-01",
            ))),
            ..blind_since(&live, since)
        };
        app.state.update(cx, |state, cx| {
            let mut status = ConnectionStatus::starting("master-01", Some("icygui".to_owned()));
            status.on_state(ConnectionState::Connected {
                node: crate::app_state::connection::full_node("master-01"),
                version: "r2.15.6-1".to_owned(),
                since,
            });
            status.on_snapshot_at(&blind, now);
            state.set_connection(status);
            state.set_snapshot(Arc::new(blind.clone()));
            cx.notify();
        });
        app.draw(cx);
        let check = |app: &Harness, cx: &mut App, page: &str| {
            app.draw(cx);
            let state = app.state.read(cx);
            let notice = state.connection_notice(now).expect(page);
            assert_eq!(notice.kind, NoticeKind::Blind, "{page}");
            assert_eq!(
                notice.title, "no live data for 3m — states may be outdated",
                "{page}"
            );
            let (_, age) = state.connection().label_parts(now);
            assert_eq!(age.as_deref(), Some("no data 3m"), "{page}");
            assert_eq!(state.connection().health(now), Health::Stale, "{page}");
        };
        check(app, cx, "a dashboard");
        for entry in [
            ClusterEntry::Health,
            ClusterEntry::Handling,
            ClusterEntry::Downtimes,
            ClusterEntry::Events,
        ] {
            app.state.update(cx, |state, cx| {
                state.show_cluster(entry);
                cx.notify();
            });
            check(app, cx, &format!("{entry:?}"));
        }
        // No false green on the health page: the last known states keep
        // their words, but no dot is green and the line says as of when.
        app.state.update(cx, |state, cx| {
            state.show_cluster(ClusterEntry::Health);
            cx.notify();
        });
        app.draw(cx);
        let page = app.workspace.read(cx).health_page().clone();
        let report = page.read(cx).report(cx);
        assert!(!report.live.is_live(), "{:?}", report.live);
        assert!(!report.zones.is_empty());
        for zone in &report.zones {
            assert_ne!(zone.dot, Dot::Ok, "{}", zone.name);
            for endpoint in &zone.endpoints {
                assert_ne!(endpoint.dot, Dot::Ok, "{}", endpoint.name);
            }
        }
        assert_ne!(report.beats.tone, BeatTone::Ok);
        endpoint_statuses_are_last_known(app, cx, now, since);
        let tray = crate::background::tray::tray_view(app.state.read(cx), now);
        assert!(tray.blind);
        assert!(
            tray.statuses
                .iter()
                .any(|(_, status)| status == "no data 3m"),
            "{:?}",
            tray.statuses
        );

        // Live again: the banner and the footer's warning go.
        app.state.update(cx, |state, cx| {
            let mut status = state.connection().clone();
            let mut fresh = (*live).clone();
            fresh.last_event_at = Some(now);
            status.on_snapshot_at(&fresh, now);
            state.set_connection(status);
            cx.notify();
        });
        app.draw(cx);
        let state = app.state.read(cx);
        assert!(state.connection_notice(now).is_none());
        assert_ne!(state.connection().health(now), Health::Stale);
        assert!(!crate::background::tray::tray_view(state, now).blind);
    });
}

/// H: the health page's `···` → *edit page* opens the dashboard editor
/// on the page's own kinds (06's layout); a view switched off is left out
/// of the page once saved, and *reset to default* brings the layout back
/// (the settings then leave the page out). A normal dashboard's
/// *add view* never offers the health kinds.
#[test]
fn the_health_page_is_edited_as_a_built_in_dashboard() {
    run(FixtureOptions::default(), |app, cx| {
        // A normal dashboard's add-view menu has no health kind.
        assert!(
            crate::editor::model::DISPLAY_SECTIONS
                .iter()
                .flat_map(|(_, displays)| displays.iter())
                .all(|display| !display.is_health())
        );
        show_health(app, cx);
        let page = app.workspace.read(cx).health_page().clone();
        let kinds = |app: &Harness, cx: &App| -> Vec<ViewDisplay> {
            let page = app.workspace.read(cx).health_page().clone();
            page.read(cx)
                .views(cx)
                .iter()
                .map(|view| view.display)
                .collect()
        };
        assert_eq!(
            kinds(app, cx),
            [
                ViewDisplay::ZonesAndEndpoints,
                ViewDisplay::Checks,
                ViewDisplay::QueuesAndConnections,
                ViewDisplay::GlobalSwitches
            ],
            "06's layout"
        );
        page.update(cx, |_, cx| cx.emit(HealthPageEvent::Edit));
        app.draw(cx);
        let edit = editor(app, cx);
        assert!(edit.read(cx).edits_health());
        assert_eq!(edit.read(cx).draft().views, HealthPage::default().views);
        assert!(
            edit.read(cx)
                .draft()
                .views
                .iter()
                .all(|view| view.display.is_health())
        );

        // Icinga's global switches off, saved: the page leaves them out.
        let switches = edit.read(cx).draft().views[3].id.clone();
        edit.update(cx, |editor, cx| {
            editor.select_view(&switches, cx);
            editor.change_selected_for_test(cx, |view| view.health.off = true);
        });
        app.draw(cx);
        app.keys(cx, "ctrl-s");
        assert!(
            app.workspace.read(cx).editor().is_none(),
            "saved and closed"
        );
        assert_eq!(
            kinds(app, cx),
            [
                ViewDisplay::ZonesAndEndpoints,
                ViewDisplay::Checks,
                ViewDisplay::QueuesAndConnections
            ]
        );
        assert_ne!(
            app.state.read(cx).environment().unwrap().health_page,
            HealthPage::default()
        );

        // Reset to default, saved: 06's layout again.
        show_health(app, cx);
        let page = app.workspace.read(cx).health_page().clone();
        page.update(cx, |_, cx| cx.emit(HealthPageEvent::Edit));
        app.draw(cx);
        let edit = editor(app, cx);
        edit.update(cx, DashboardEditor::reset_health_page_for_test);
        app.draw(cx);
        assert_eq!(edit.read(cx).draft().views, HealthPage::default().views);
        app.keys(cx, "ctrl-s");
        assert!(app.workspace.read(cx).editor().is_none());
        assert_eq!(kinds(app, cx).len(), 4);
        assert_eq!(
            app.state.read(cx).environment().unwrap().health_page,
            HealthPage::default()
        );
    });
}

/// The health page by keyboard alone (a dashboard's keys): the cursor
/// moves over the links, the views' headers, the zones and the endpoints;
/// Enter on an alert's *show sat-fra-01* puts the cursor on that row;
/// `←`/`→` fold and unfold the view, `tab` jumps to the next view, `End`
/// to the last stop, `Esc` leaves the cursor.
#[test]
fn the_health_page_works_by_keyboard_alone() {
    run(FixtureOptions::default(), |app, cx| {
        let now = Timestamp::now();
        app.state.update(cx, |state, cx| {
            let snapshot = Snapshot {
                trouble: Arc::new(Trouble {
                    alerts: vec![Alert {
                        key: "endpoint:sat-fra-01".to_owned(),
                        tone: AlertTone::Critical,
                        title: "zone fra: sat-fra-01 disconnected".to_owned(),
                        detail: String::new(),
                        action: Some(AlertAction::ShowNode("sat-fra-01".to_owned())),
                        since: now,
                    }],
                    blind: None,
                }),
                ..with_zones(state.snapshot())
            };
            state.set_snapshot(Arc::new(snapshot));
            state.show_cluster(ClusterEntry::Health);
            cx.notify();
        });
        app.draw(cx);
        let page = app.workspace.read(cx).health_page().clone();
        app.in_window(cx, |window, cx| {
            let handle = gpui::Focusable::focus_handle(page.read(cx), cx);
            window.focus(&handle, cx);
        });
        app.draw(cx);
        let cursor = |cx: &App| page.read(cx).cursor().cloned();
        app.keys(cx, "j");
        assert_eq!(cursor(cx), Some(HealthStop::BeatSettings));
        app.keys(cx, "down");
        assert_eq!(cursor(cx), Some(HealthStop::AlertLink));
        // The alert's link: the node's row, unfolded and under the cursor.
        app.keys(cx, "enter");
        app.draw(cx);
        assert_eq!(
            cursor(cx),
            Some(HealthStop::Endpoint("sat-fra-01".to_owned()))
        );
        // ← folds the view the cursor is in, the cursor going up to its
        // header; the next stop is then the next view's header.
        app.keys(cx, "left");
        app.draw(cx);
        assert_eq!(
            cursor(cx),
            Some(HealthStop::Header(ViewDisplay::ZonesAndEndpoints))
        );
        app.keys(cx, "j");
        assert_eq!(cursor(cx), Some(HealthStop::Header(ViewDisplay::Checks)));
        // Back up, → unfolds it: its first zone follows the header.
        app.keys(cx, "k");
        app.keys(cx, "right");
        app.draw(cx);
        app.keys(cx, "j");
        assert!(
            matches!(cursor(cx), Some(HealthStop::Zone(_))),
            "{:?}",
            cursor(cx)
        );
        // Tab: the next view's header; End: the last stop; Esc: none.
        app.keys(cx, "tab");
        assert_eq!(cursor(cx), Some(HealthStop::Header(ViewDisplay::Checks)));
        app.keys(cx, "end");
        app.draw(cx);
        assert_eq!(
            cursor(cx),
            Some(HealthStop::Header(ViewDisplay::GlobalSwitches))
        );
        // Enter on a header folds it.
        app.keys(cx, "enter");
        app.draw(cx);
        app.keys(cx, "escape");
        assert_eq!(cursor(cx), None);
    });
}
