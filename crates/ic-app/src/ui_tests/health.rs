//! The cluster health page (topic 06) through the real core and the
//! demo's `prod-cluster` (two masters, a satellite in `ams` and one in
//! `fra`): it opens from the sidebar, the switcher and the palette; while
//! it shows, the engine asks for the node's listener status with every
//! poll (closed, every 5 minutes for the trouble alerts); the satellite
//! dropping out turns the page and the sidebar's dot critical.

use std::time::Duration;

use futures::FutureExt as _;
use ic_core::NodeState;
use ic_model::{FeatureState, Timestamp};

use super::environments::{CONNECT, connected_to};
use super::live::control;
use super::{Body, run_app, wait_for};
use crate::cluster::health::Tone;
use crate::cluster::{ClusterEntry, ClusterState};
use crate::live::demo;

/// The demo's `prod-cluster`, as the live tests run it.
fn demo_app() -> impl FnOnce(&mut gpui::App) -> gpui::Entity<crate::app_state::AppState> + 'static {
    super::environments::demo_app(None)
}

/// How many times the demo's master was asked for `path`.
fn asked(cx: &mut gpui::App, path: &str) -> usize {
    control(cx)
        .requests()
        .iter()
        .filter(|request| request.path == path)
        .count()
}

#[test]
#[expect(clippy::too_many_lines, reason = "one story, step by step")]
fn the_health_page_shows_the_cluster_and_asks_only_while_open() {
    run_app(
        crate::WINDOW_SIZE,
        demo_app(),
        Body::Async(Box::new(|app, cx| {
            async move {
                wait_for(&app, &cx, "prod-cluster", CONNECT, |app, cx| {
                    connected_to(app.state.read(cx), demo::ENDPOINT)
                })
                .await;
                // Closed: the trouble alerts' round only (PLAN.md §4.2 E2:
                // every 5 minutes, with a status poll).
                cx.update(|cx| {
                    assert!(asked(cx, "/v1/status/ApiListener") <= 1);
                    assert!(asked(cx, "/v1/objects/checkercomponents") <= 1);
                });
                // Opened from the palette's command (as from the sidebar
                // and the switcher: the same entry).
                cx.update(|cx| {
                    app.state.update(cx, |state, cx| {
                        assert!(state.show_cluster(ClusterEntry::Health));
                        cx.notify();
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(app.workspace.read(cx).shown_page(), "health");
                });
                wait_for(
                    &app,
                    &cx,
                    "the listener and the features",
                    CONNECT,
                    |app, cx| {
                        let health = &app.state.read(cx).snapshot().health;
                        health.listener.is_some() && health.features.is_some()
                    },
                )
                .await;
                cx.update(|cx| {
                    let listener = asked(cx, "/v1/status/ApiListener");
                    assert!(
                        (1..=2).contains(&listener),
                        "{listener}: at once, unless just asked"
                    );
                    assert_eq!(
                        asked(cx, "/v1/objects/checkercomponents"),
                        1,
                        "every 5 minutes, open or not"
                    );
                    let page = app.workspace.read(cx).health_page().clone();
                    let report = page.read(cx).report(cx);
                    assert_eq!(report.seen_from.as_deref(), Some("master-01"));
                    let zones: Vec<&str> =
                        report.zones.iter().map(|zone| zone.name.as_str()).collect();
                    assert_eq!(zones, ["master", "ams", "fra"]);
                    assert_eq!((report.connected, report.not_connected), (4, 0));
                    assert_eq!(report.global_zones, ["director-global", "global-templates"]);
                    assert!(report.zones[0].endpoints[0].this_node);
                    let features: Vec<(&str, FeatureState)> = report
                        .features
                        .iter()
                        .map(|feature| (feature.label, feature.state))
                        .collect();
                    assert_eq!(
                        features,
                        [
                            ("checker", FeatureState::Running),
                            ("notification", FeatureState::Running),
                            ("IcingaDB", FeatureState::Off)
                        ]
                    );
                    assert_eq!(report.queues[2].value, "3 of 3");
                    assert!(report.queues.last().unwrap().wide, "no IcingaDB tile");
                    assert!(!report.checks.is_empty());
                    assert_ne!(report.state, ClusterState::Critical);
                });

                // The satellite in `fra` drops out: once the cluster
                // nodes' states come (the page asks for them now), its
                // zone is cut off and the sidebar's dot turns critical.
                cx.update(|cx| {
                    control(cx)
                        .set_endpoint_connected("sat-fra-01", false)
                        .unwrap();
                    app.state.update(cx, |state, cx| {
                        if state.refresh(std::time::Instant::now()) {
                            cx.notify();
                        }
                    });
                });
                wait_for(
                    &app,
                    &cx,
                    "sat-fra-01 down",
                    Duration::from_secs(90),
                    |app, cx| {
                        app.state
                            .read(cx)
                            .snapshot()
                            .cluster_nodes()
                            .iter()
                            .any(|node| {
                                node.name == "sat-fra-01" && node.state == NodeState::Disconnected
                            })
                    },
                )
                .await;
                cx.update(|cx| {
                    let page = app.workspace.read(cx).health_page().clone();
                    let report = page.read(cx).report(cx);
                    assert_eq!(report.state, ClusterState::Critical);
                    assert_eq!((report.connected, report.not_connected), (3, 1));
                    let fra = &report.zones[2].endpoints[0];
                    assert_eq!(fra.tone, Tone::Critical);
                    // Its alert is raised after the 2-minute grace (the
                    // engine's tests follow it there).
                    assert!(
                        report
                            .alerts
                            .iter()
                            .all(|alert| alert.tone == Tone::Critical)
                    );
                    let state = app.state.read(cx);
                    assert_eq!(
                        crate::cluster::cluster_state(
                            state.snapshot(),
                            state.connection().is_connected(),
                            Timestamp::now()
                        ),
                        ClusterState::Critical
                    );
                });

                // Closed again (another page shown): the engine is told.
                cx.update(|cx| {
                    assert!(app.state.read(cx).health_page_shown());
                    app.state.update(cx, |state, cx| {
                        assert!(state.show_cluster(ClusterEntry::Events));
                        cx.notify();
                    });
                });
                cx.update(|cx| {
                    app.draw(cx);
                    assert_eq!(app.workspace.read(cx).shown_page(), "events");
                    assert!(!app.state.read(cx).health_page_shown());
                });
            }
            .boxed_local()
        })),
    );
}
